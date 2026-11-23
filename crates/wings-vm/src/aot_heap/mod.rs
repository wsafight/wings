use std::alloc::{Layout, alloc};

use wings_abi::{
    HEAP_COLLECT, HEAP_EMPTY_INTS, HEAP_HEAD_INT, HEAP_IS_EMPTY_INTS, HEAP_PREPEND_INT,
    HEAP_TAIL_INTS, MAX_SHADOW_ROOTS, STATUS_EMPTY_LIST, STATUS_HEAP_EXHAUSTED,
    STATUS_INVALID_CONTROL, STATUS_INVALID_ROOT, STATUS_OK, STATUS_REPLY_REJECTED, ScalarContext,
    ShadowFrame,
};

use crate::message_values::{BytePayload, ListPayload, MAX_MESSAGE_BYTES};
use crate::{
    FieldType, HeapRef, HeapStats, RecordHeap, RecordLayout, RecordReply, RecordValue, Reservation,
    ResourceAccount, Resources, ShadowRoots,
};

pub(crate) struct AotHeap {
    pub(crate) account: ResourceAccount,
    heap: RecordHeap,
    roots: ShadowRoots,
    empty_list: RecordLayout,
    cons_list: RecordLayout,
    committed: Option<HeapRef>,
    incoming_mask: u64,
    incoming: Option<Box<[Option<HeapRef>; 64]>>,
    named: Box<[NamedLayout]>,
    _named_charge: Reservation,
    _metadata: Reservation,
}

mod ops;
#[cfg(test)]
mod tests;
mod transfer;

pub(crate) use ops::heap_call;
use ops::string_literal;
#[cfg(test)]
pub(crate) use transfer::export_bytes;
pub(crate) use transfer::{export_buffers, export_lists, export_record_reply, import_arguments};

#[derive(Clone, Copy)]
struct NamedLayout {
    layout: RecordLayout,
    types: [wings_abi::ValueType; 8],
    count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShadowRootError {
    Invalid,
    Exhausted,
}

impl AotHeap {
    pub(crate) fn byte_state_snapshot(&self) -> Option<crate::ByteStateSnapshot> {
        let bytes = self.heap.bytes(self.committed?).ok()?;
        let mut prefix = [0; 32];
        let count = bytes.len().min(prefix.len());
        prefix[..count].copy_from_slice(&bytes[..count]);
        Some(crate::ByteStateSnapshot {
            length: bytes.len(),
            prefix,
        })
    }

    #[cfg(test)]
    pub(crate) fn new(account: &ResourceAccount, capacity: usize) -> Result<Box<Self>, String> {
        Self::new_with_records(account, capacity, &[])
    }

    pub(crate) fn new_with_records(
        account: &ResourceAccount,
        capacity: usize,
        records: &[wings_abi::RecordSchema],
    ) -> Result<Box<Self>, String> {
        if records.len() > 32 {
            return Err("record schema count exceeds 32".into());
        }
        let mut heap = RecordHeap::new(account, capacity, 2 + records.len())?;
        let metadata = account.reserve(Resources {
            bytes: (std::mem::size_of::<Self>() + std::mem::size_of::<[Option<HeapRef>; 64]>()
                - std::mem::size_of::<RecordHeap>()
                - std::mem::size_of::<ShadowRoots>()) as u64,
            ..Resources::default()
        })?;
        let named_charge = account.reserve(Resources {
            bytes: (records.len() * std::mem::size_of::<NamedLayout>()) as u64,
            ..Resources::default()
        })?;
        let empty_list = heap.register_layout(&[FieldType::Unit])?;
        let cons_list = heap.register_layout(&[FieldType::Int, FieldType::Ref])?;
        let mut named = Vec::new();
        named
            .try_reserve_exact(records.len())
            .map_err(|_| "named record layout allocation failed")?;
        if named.capacity() != records.len() {
            return Err("allocator changed named layout capacity".into());
        }
        for record in records {
            let mut types = [wings_abi::ValueType::Unit; 8];
            let mut fields = [FieldType::Unit; 8];
            if record.fields.len() > 8
                || record.fields.iter().any(|(_, ty)| {
                    *ty == wings_abi::ValueType::RequestId
                        || matches!(ty, wings_abi::ValueType::RecordRef(index) if usize::from(*index) >= named.len())
                })
            {
                return Err("invalid named record layout fields".into());
            }
            for (index, (_, ty)) in record.fields.iter().enumerate() {
                types[index] = *ty;
                fields[index] = if ty.is_reference() {
                    FieldType::Ref
                } else {
                    match ty {
                        wings_abi::ValueType::Int | wings_abi::ValueType::Pid => FieldType::Int,
                        wings_abi::ValueType::Bool => FieldType::Bool,
                        _ => FieldType::Unit,
                    }
                };
            }
            let layout = heap.register_distinct_layout(&fields[..record.fields.len()])?;
            named.push(NamedLayout {
                layout,
                types,
                count: record.fields.len(),
            });
        }
        let roots = ShadowRoots::with_state_root(&heap)?;
        let pointer = unsafe { alloc(Layout::new::<Self>()) }.cast::<Self>();
        if pointer.is_null() {
            return Err("AOT heap control allocation failed".into());
        }
        unsafe {
            pointer.write(Self {
                account: account.clone(),
                heap,
                roots,
                empty_list,
                cons_list,
                committed: None,
                incoming_mask: 0,
                incoming: None,
                named: named.into_boxed_slice(),
                _named_charge: named_charge,
                _metadata: metadata,
            })
        };
        Ok(unsafe { Box::from_raw(pointer) })
    }

    pub(crate) fn stats(&self) -> HeapStats {
        self.heap.stats()
    }

    pub(crate) fn committed_bytes_len(&self) -> Option<usize> {
        let reference = self.committed?;
        self.heap.bytes(reference).ok().map(<[u8]>::len)
    }

    pub(crate) fn commit_list(&mut self, encoded: i64) -> Result<(), String> {
        let reference = self.list_reference(encoded as u64)?;
        self.committed = Some(reference);
        Ok(())
    }

    pub(crate) fn commit_typed(
        &mut self,
        ty: wings_abi::ValueType,
        encoded: i64,
    ) -> Result<(), String> {
        if let wings_abi::ValueType::RecordRef(_) = ty {
            let reference = self
                .typed_reference(ty, encoded)
                .map_err(|_| "invalid named record state")?;
            self.committed = Some(reference);
            Ok(())
        } else if ty == wings_abi::ValueType::StringRef {
            let reference = self.heap.import_reference(encoded as u64)?;
            self.heap.string_bytes(reference)?;
            let previous = self.committed.replace(reference);
            if let Some(old_ref) = previous {
                if old_ref != reference && self.incoming_mask == 0 && self.roots.is_empty() {
                    self.heap.try_recycle_sole_leaf(old_ref, reference);
                }
            }
            Ok(())
        } else if ty == wings_abi::ValueType::BytesRef {
            let reference = self.heap.import_reference(encoded as u64)?;
            self.heap.bytes(reference)?;
            let previous = self.committed.replace(reference);
            if let Some(old_ref) = previous {
                if old_ref != reference && self.incoming_mask == 0 && self.roots.is_empty() {
                    self.heap.try_recycle_sole_leaf(old_ref, reference);
                }
            }
            Ok(())
        } else if ty == wings_abi::ValueType::ListRef {
            self.commit_list(encoded)
        } else {
            Err("invalid heap state type".into())
        }
    }

    fn typed_reference(&self, ty: wings_abi::ValueType, encoded: i64) -> Result<HeapRef, u32> {
        let reference = self
            .heap
            .import_reference(encoded as u64)
            .map_err(|_| STATUS_INVALID_ROOT)?;
        let valid = match ty {
            wings_abi::ValueType::ListRef => {
                return self
                    .list_reference(encoded as u64)
                    .map_err(|_| STATUS_INVALID_ROOT);
            }
            wings_abi::ValueType::BytesRef => self.heap.bytes(reference).map(|_| true),
            wings_abi::ValueType::StringRef => self.heap.string_bytes(reference).map(|_| true),
            wings_abi::ValueType::RecordRef(index) => self.heap.has_layout(
                reference,
                self.named
                    .get(usize::from(index))
                    .ok_or(STATUS_INVALID_ROOT)?
                    .layout,
            ),
            _ => return Err(STATUS_INVALID_ROOT),
        }
        .map_err(|_| STATUS_INVALID_ROOT)?;
        if valid {
            Ok(reference)
        } else {
            Err(STATUS_INVALID_ROOT)
        }
    }

    fn list_reference(&self, encoded: u64) -> Result<HeapRef, String> {
        let reference = self.heap.import_reference(encoded)?;
        if !self.heap.has_layout(reference, self.empty_list)?
            && !self.heap.has_layout(reference, self.cons_list)?
        {
            return Err("expected an IntList node".into());
        }
        Ok(reference)
    }

    unsafe fn prepare_roots_checked(
        &mut self,
        mut frame: *const ShadowFrame,
    ) -> Result<(), ShadowRootError> {
        let mut references: [std::mem::MaybeUninit<Option<HeapRef>>; MAX_SHADOW_ROOTS + 65] =
            [const { std::mem::MaybeUninit::uninit() }; MAX_SHADOW_ROOTS + 65];
        let mut count = 0;
        let mut depth = 0;
        let mut slow = frame;
        let mut advance_slow = false;
        while !frame.is_null() {
            let current = unsafe { &*frame };
            if current.count > MAX_SHADOW_ROOTS || (current.count != 0 && current.slots.is_null()) {
                return Err(ShadowRootError::Invalid);
            }
            if current.previous == frame {
                return Err(ShadowRootError::Invalid);
            }
            if depth == MAX_SHADOW_ROOTS || current.count > MAX_SHADOW_ROOTS - count {
                return Err(ShadowRootError::Exhausted);
            }
            for index in 0..current.count {
                let encoded = unsafe { *current.slots.add(index) };
                references[count].write(Some(
                    self.heap
                        .import_reference(encoded)
                        .map_err(|_| ShadowRootError::Invalid)?,
                ));
                count += 1;
            }
            depth += 1;
            frame = current.previous;
            if advance_slow {
                slow = unsafe { (*slow).previous };
                if !frame.is_null() && slow == frame {
                    return Err(ShadowRootError::Invalid);
                }
            }
            advance_slow = !advance_slow;
        }
        if let Some(committed) = self.committed {
            self.heap
                .import_reference(committed.encoded())
                .map_err(|_| ShadowRootError::Invalid)?;
            references[count].write(Some(committed));
            count += 1;
        }
        let mut incoming_bits = self.incoming_mask;
        if incoming_bits != 0 {
            let incoming = self.incoming.as_ref().ok_or(ShadowRootError::Invalid)?;
            while incoming_bits != 0 {
                let slot = incoming_bits.trailing_zeros() as usize;
                incoming_bits &= incoming_bits - 1;
                if let Some(reference) = incoming[slot] {
                    self.heap
                        .import_reference(reference.encoded())
                        .map_err(|_| ShadowRootError::Invalid)?;
                    references[count].write(Some(reference));
                    count += 1;
                }
            }
        }
        let initialized = unsafe {
            std::slice::from_raw_parts(references.as_ptr().cast::<Option<HeapRef>>(), count)
        };
        self.roots
            .try_replace(initialized)
            .map_err(|_| ShadowRootError::Exhausted)?;
        Ok(())
    }

    #[cfg(test)]
    unsafe fn prepare_roots(&mut self, frame: *const ShadowFrame) -> Result<(), String> {
        match unsafe { self.prepare_roots_checked(frame) } {
            Ok(()) => Ok(()),
            Err(ShadowRootError::Exhausted) => {
                Err("shadow frame depth or root capacity exhausted".into())
            }
            Err(ShadowRootError::Invalid) => Err("invalid shadow root count or slots".into()),
        }
    }

    #[cfg(test)]
    unsafe fn collect(&mut self, frame: *const ShadowFrame) -> Result<(), String> {
        unsafe { self.prepare_roots(frame) }?;
        self.heap.collect(&self.roots)?;
        Ok(())
    }

    #[inline]
    pub(crate) fn clear_incoming(&mut self) {
        let mut bits = self.incoming_mask;
        if bits != 0 {
            if let Some(incoming) = self.incoming.as_mut() {
                while bits != 0 {
                    let slot = bits.trailing_zeros() as usize;
                    bits &= bits - 1;
                    incoming[slot] = None;
                }
            }
            self.incoming_mask = 0;
        }
    }

    fn list_node(&self, encoded: i64) -> Result<Option<(i64, i64)>, u32> {
        let reference = self
            .list_reference(encoded as u64)
            .map_err(|_| STATUS_INVALID_ROOT)?;
        if self
            .heap
            .has_layout(reference, self.empty_list)
            .map_err(|_| STATUS_INVALID_ROOT)?
        {
            return Ok(None);
        }
        let RecordValue::Int(value) = self
            .heap
            .get(reference, 0)
            .map_err(|_| STATUS_INVALID_ROOT)?
        else {
            return Err(STATUS_INVALID_ROOT);
        };
        let RecordValue::Ref(tail) = self
            .heap
            .get(reference, 1)
            .map_err(|_| STATUS_INVALID_ROOT)?
        else {
            return Err(STATUS_INVALID_ROOT);
        };
        Ok(Some((value, tail.encoded() as i64)))
    }
}

pub(super) unsafe fn collect_managed(context: &mut ScalarContext) -> Result<(), u32> {
    unsafe { collect_managed_recycling(context, 0) }.map(|_| ())
}

pub(super) unsafe fn collect_managed_recycling(
    context: &mut ScalarContext,
    recycle_len: usize,
) -> Result<Option<crate::heap::ByteBuffer>, u32> {
    let pointer = context.heap_data.cast::<AotHeap>();
    let mut collection = {
        let heap = unsafe { &mut *pointer };
        unsafe { heap.prepare_roots_checked(context.roots) }.map_err(|err| match err {
            ShadowRootError::Exhausted => wings_abi::STATUS_STACK_EXHAUSTED,
            ShadowRootError::Invalid => STATUS_INVALID_ROOT,
        })?;
        heap.heap
            .begin_collection(&heap.roots)
            .map_err(|_| STATUS_INVALID_ROOT)?
    };
    let mut recycled = None;
    loop {
        crate::execution::runtime_checkpoint(context, true)?;
        let finished = {
            let heap = unsafe { &mut *pointer };
            heap.heap
                .collect_chunk_recycling(&heap.roots, &mut collection, recycle_len, &mut recycled)
                .is_some()
        };
        if finished {
            return Ok(recycled);
        }
    }
}

pub(super) unsafe fn allocate_managed(
    context: &mut ScalarContext,
    layout: RecordLayout,
    values: &[RecordValue],
) -> Result<u64, u32> {
    let pointer = context.heap_data.cast::<AotHeap>();
    for attempt in 0..2 {
        let mut cursor = 0;
        loop {
            if cursor != 0 {
                crate::execution::runtime_checkpoint(context, false)?;
            }
            match unsafe { &mut *pointer }
                .heap
                .allocate_chunk(layout, values, &mut cursor)
            {
                Ok(Some(reference)) => return Ok(reference.encoded()),
                Ok(None) => {}
                Err(_) => break,
            }
        }
        if attempt == 0 {
            unsafe { collect_managed(context) }?;
        }
    }
    Err(STATUS_HEAP_EXHAUSTED)
}
