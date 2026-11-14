use std::alloc::{Layout, alloc};

use wings_abi::{
    HEAP_COLLECT, HEAP_EMPTY_INTS, HEAP_HEAD_INT, HEAP_IS_EMPTY_INTS, HEAP_PREPEND_INT,
    HEAP_READ_INT, HEAP_RECORD_INT, HEAP_TAIL_INTS, MAX_SHADOW_ROOTS, STATUS_EMPTY_LIST,
    STATUS_HEAP_EXHAUSTED, STATUS_INVALID_CONTROL, STATUS_INVALID_ROOT, STATUS_OK, ScalarContext,
    ShadowFrame,
};

use crate::message_values::{BytePayload, ListPayload, MAX_LIST_ELEMENTS, MAX_MESSAGE_BYTES};
use crate::{
    FieldType, HeapRef, HeapStats, RecordHeap, RecordLayout, RecordValue, Reservation,
    ResourceAccount, Resources, ShadowRoots,
};

pub(crate) struct AotHeap {
    pub(crate) account: ResourceAccount,
    heap: RecordHeap,
    roots: ShadowRoots,
    layout: RecordLayout,
    empty_list: RecordLayout,
    cons_list: RecordLayout,
    committed: Option<HeapRef>,
    incoming: [Option<HeapRef>; 64],
    _metadata: Reservation,
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

    pub(crate) fn new(account: &ResourceAccount, capacity: usize) -> Result<Box<Self>, String> {
        let mut heap = RecordHeap::new(account, capacity, 3)?;
        let metadata = account.reserve(Resources {
            bytes: (std::mem::size_of::<Self>()
                - std::mem::size_of::<RecordHeap>()
                - std::mem::size_of::<ShadowRoots>()) as u64,
            ..Resources::default()
        })?;
        let layout = heap.register_layout(&[FieldType::Int])?;
        let empty_list = heap.register_layout(&[FieldType::Unit])?;
        let cons_list = heap.register_layout(&[FieldType::Int, FieldType::Ref])?;
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
                layout,
                empty_list,
                cons_list,
                committed: None,
                incoming: [None; 64],
                _metadata: metadata,
            })
        };
        Ok(unsafe { Box::from_raw(pointer) })
    }

    pub(crate) fn stats(&self) -> HeapStats {
        self.heap.stats()
    }

    pub(crate) fn commit(&mut self, encoded: i64) -> Result<(), String> {
        let reference = self.heap.import_reference(encoded as u64)?;
        if !self.heap.has_layout(reference, self.layout)? {
            return Err("record state requires an Int field".into());
        }
        self.committed = Some(reference);
        Ok(())
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
        if ty == wings_abi::ValueType::BytesRef {
            let reference = self.heap.import_reference(encoded as u64)?;
            self.heap.bytes(reference)?;
            self.committed = Some(reference);
            Ok(())
        } else if ty == wings_abi::ValueType::ListRef {
            self.commit_list(encoded)
        } else {
            self.commit(encoded)
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

    unsafe fn prepare_roots(&mut self, mut frame: *const ShadowFrame) -> Result<(), String> {
        let mut references = [None; MAX_SHADOW_ROOTS + 65];
        let mut count = 0;
        let mut depth = 0;
        while !frame.is_null() {
            if depth == MAX_SHADOW_ROOTS {
                return Err("shadow frame depth exceeds 64".into());
            }
            let current = unsafe { &*frame };
            if current.count > MAX_SHADOW_ROOTS - count
                || (current.count != 0 && current.slots.is_null())
            {
                return Err("invalid shadow root count or slots".into());
            }
            for index in 0..current.count {
                let encoded = unsafe { *current.slots.add(index) };
                references[count] = Some(self.heap.import_reference(encoded)?);
                count += 1;
            }
            depth += 1;
            frame = current.previous;
        }
        if let Some(committed) = self.committed {
            self.heap.import_reference(committed.encoded())?;
            references[count] = Some(committed);
            count += 1;
        }
        for reference in self.incoming.iter().flatten() {
            self.heap.import_reference(reference.encoded())?;
            references[count] = Some(*reference);
            count += 1;
        }
        self.roots.replace(&references[..count]);
        Ok(())
    }

    #[cfg(test)]
    unsafe fn collect(&mut self, frame: *const ShadowFrame) -> Result<(), String> {
        unsafe { self.prepare_roots(frame) }?;
        self.heap.collect(&self.roots)?;
        Ok(())
    }

    pub(crate) fn clear_incoming(&mut self) {
        self.incoming.fill(None);
    }

    pub(crate) fn record_value(&self, encoded: i64) -> Result<i64, u32> {
        let reference = self
            .heap
            .import_reference(encoded as u64)
            .map_err(|_| STATUS_INVALID_ROOT)?;
        if !self
            .heap
            .has_layout(reference, self.layout)
            .map_err(|_| STATUS_INVALID_ROOT)?
        {
            return Err(STATUS_INVALID_ROOT);
        }
        match self
            .heap
            .get(reference, 0)
            .map_err(|_| STATUS_INVALID_ROOT)?
        {
            RecordValue::Int(value) => Ok(value),
            _ => Err(STATUS_INVALID_ROOT),
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

pub(crate) unsafe fn export_lists(
    context: &mut ScalarContext,
    arguments: &mut [i64],
    mask: u64,
) -> Result<ListPayload, u32> {
    let mut payload = ListPayload {
        mask,
        ..ListPayload::default()
    };
    for (index, argument) in arguments.iter_mut().enumerate() {
        if mask & (1u64 << index) == 0 {
            continue;
        }
        let start = payload.count;
        let mut current = *argument;
        loop {
            crate::execution::runtime_checkpoint(context, true)?;
            let heap = unsafe { context.heap_data.cast::<AotHeap>().as_ref() }
                .ok_or(STATUS_INVALID_ROOT)?;
            let Some((value, tail)) = heap.list_node(current)? else {
                break;
            };
            if payload.count == MAX_LIST_ELEMENTS {
                return Err(wings_abi::STATUS_SEND_REJECTED);
            }
            payload.values[payload.count] = value;
            payload.count += 1;
            current = tail;
        }
        *argument = ListPayload::encode(start, payload.count - start);
    }
    Ok(payload)
}

pub(crate) unsafe fn export_bytes(
    context: &mut ScalarContext,
    arguments: &mut [i64],
    mask: u64,
) -> Result<BytePayload, u32> {
    if mask == 0 {
        return Ok(BytePayload::default());
    }
    let pointer = context.heap_data.cast::<AotHeap>();
    let mut length = 0usize;
    for (index, argument) in arguments.iter().enumerate() {
        if mask & (1u64 << index) != 0 {
            crate::execution::runtime_checkpoint(context, true)?;
            let heap = unsafe { pointer.as_ref() }.ok_or(STATUS_INVALID_ROOT)?;
            let reference = heap
                .heap
                .import_reference(*argument as u64)
                .map_err(|_| STATUS_INVALID_ROOT)?;
            let bytes = heap
                .heap
                .bytes(reference)
                .map_err(|_| STATUS_INVALID_ROOT)?;
            length = length
                .checked_add(bytes.len())
                .filter(|total| *total <= MAX_MESSAGE_BYTES)
                .ok_or(wings_abi::STATUS_SEND_REJECTED)?;
        }
    }
    let account = unsafe { &*pointer }.account.clone();
    let mut payload =
        BytePayload::new(&account, mask, length).map_err(|_| STATUS_HEAP_EXHAUSTED)?;
    for (index, argument) in arguments.iter_mut().enumerate() {
        if mask & (1u64 << index) == 0 {
            continue;
        }
        let reference = unsafe { &*pointer }
            .heap
            .import_reference(*argument as u64)
            .map_err(|_| STATUS_INVALID_ROOT)?;
        let length = unsafe { &*pointer }
            .heap
            .bytes(reference)
            .map_err(|_| STATUS_INVALID_ROOT)?
            .len();
        let start = payload.data.len();
        for offset in (0..length).step_by(256) {
            crate::execution::runtime_checkpoint(context, true)?;
            let bytes = unsafe { &*pointer }
                .heap
                .bytes(reference)
                .map_err(|_| STATUS_INVALID_ROOT)?;
            payload
                .data
                .extend_from_slice(&bytes[offset..(offset + 256).min(length)]);
        }
        *argument = ListPayload::encode(start, length);
    }
    Ok(payload)
}

unsafe fn import_bytes(context: &mut ScalarContext, data: &[u8]) -> Result<u64, u32> {
    let pointer = context.heap_data.cast::<AotHeap>();
    let mut buffer = match unsafe { &*pointer }.heap.prepare_bytes(data.len()) {
        Ok(buffer) => buffer,
        Err(_) => {
            unsafe { collect_managed(context) }?;
            unsafe { &*pointer }
                .heap
                .prepare_bytes(data.len())
                .map_err(|_| STATUS_HEAP_EXHAUSTED)?
        }
    };
    for chunk in data.chunks(256) {
        crate::execution::runtime_checkpoint(context, true)?;
        buffer.data.extend_from_slice(chunk);
    }
    let mut buffer = Some(buffer);
    for attempt in 0..2 {
        let mut cursor = 0;
        loop {
            crate::execution::runtime_checkpoint(context, true)?;
            match unsafe { &mut *pointer }
                .heap
                .allocate_bytes_chunk(&mut buffer, &mut cursor)
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

pub(crate) unsafe fn import_arguments(
    context: &mut ScalarContext,
    arguments: &mut [i64],
    mask: u64,
    lists: &ListPayload,
    bytes: &BytePayload,
) -> Result<(), u32> {
    if !lists.valid(arguments, lists.mask)
        || !bytes.valid(arguments, bytes.mask)
        || mask & (lists.mask | bytes.mask) != 0
        || lists.mask & bytes.mask != 0
        || (arguments.len() < 64 && mask >> arguments.len() != 0)
    {
        return Err(STATUS_INVALID_CONTROL);
    }
    for (index, argument) in arguments.iter_mut().enumerate() {
        let bit = 1u64 << index;
        if (mask | lists.mask | bytes.mask) & bit == 0 {
            continue;
        }
        if context.heap_data.is_null() {
            return Err(STATUS_HEAP_EXHAUSTED);
        }
        crate::execution::runtime_checkpoint(context, true)?;
        if bytes.mask & bit != 0 {
            let (start, length) = ListPayload::segment(*argument);
            let encoded = unsafe { import_bytes(context, &bytes.data[start..start + length]) }?;
            unsafe { root_incoming(context, index, encoded) }?;
            *argument = encoded as i64;
            continue;
        }
        let mut encoded = if mask & bit != 0 {
            let layout = unsafe { &*context.heap_data.cast::<AotHeap>() }.layout;
            unsafe { allocate_managed(context, layout, &[RecordValue::Int(*argument)]) }?
        } else {
            let layout = unsafe { &*context.heap_data.cast::<AotHeap>() }.empty_list;
            unsafe { allocate_managed(context, layout, &[RecordValue::Unit]) }?
        };
        unsafe { root_incoming(context, index, encoded) }?;
        if lists.mask & bit != 0 {
            let (start, length) = ListPayload::segment(*argument);
            for value in lists.values[start..start + length].iter().rev() {
                crate::execution::runtime_checkpoint(context, true)?;
                let heap = unsafe { &*context.heap_data.cast::<AotHeap>() };
                let tail = heap
                    .heap
                    .import_reference(encoded)
                    .map_err(|_| STATUS_INVALID_ROOT)?;
                let layout = heap.cons_list;
                encoded = unsafe {
                    allocate_managed(
                        context,
                        layout,
                        &[RecordValue::Int(*value), RecordValue::Ref(tail)],
                    )
                }?;
                unsafe { root_incoming(context, index, encoded) }?;
            }
        }
        *argument = encoded as i64;
    }
    Ok(())
}

unsafe fn root_incoming(
    context: &mut ScalarContext,
    index: usize,
    encoded: u64,
) -> Result<(), u32> {
    let heap = unsafe { &mut *context.heap_data.cast::<AotHeap>() };
    let reference = heap
        .heap
        .import_reference(encoded)
        .map_err(|_| STATUS_INVALID_ROOT)?;
    heap.incoming[index] = Some(reference);
    Ok(())
}
unsafe fn collect_managed(context: &mut ScalarContext) -> Result<(), u32> {
    let pointer = context.heap_data.cast::<AotHeap>();
    let mut collection = {
        let heap = unsafe { &mut *pointer };
        unsafe { heap.prepare_roots(context.roots) }.map_err(|_| STATUS_INVALID_ROOT)?;
        heap.heap
            .begin_collection(&heap.roots)
            .map_err(|_| STATUS_INVALID_ROOT)?
    };
    loop {
        crate::execution::runtime_checkpoint(context, true)?;
        let finished = {
            let heap = unsafe { &mut *pointer };
            heap.heap
                .collect_chunk(&heap.roots, &mut collection)
                .is_some()
        };
        if finished {
            return Ok(());
        }
    }
}

unsafe fn allocate_managed(
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

unsafe fn byte_call(
    context: &mut ScalarContext,
    operation: u32,
    arguments: [i64; 3],
) -> Result<u64, u32> {
    use wings_abi::{
        HEAP_BYTE_AT, HEAP_BYTES_LEN, HEAP_FILL_BYTES, HEAP_READ_I64_BE, HEAP_REPLACE_I64_BE,
        STATUS_INVALID_BYTES,
    };
    let width = if operation == HEAP_READ_I64_BE || operation == HEAP_REPLACE_I64_BE {
        8
    } else {
        1
    };
    let pointer = context.heap_data.cast::<AotHeap>();
    let (length, source, index, value) = if operation == HEAP_FILL_BYTES {
        let length = usize::try_from(arguments[0])
            .ok()
            .filter(|length| *length <= crate::heap::MAX_BYTES)
            .ok_or(STATUS_INVALID_BYTES)?;
        let value = u8::try_from(arguments[1]).map_err(|_| STATUS_INVALID_BYTES)?;
        (length, None, 0, u64::from(value))
    } else {
        let heap = unsafe { &*pointer };
        let reference = heap
            .heap
            .import_reference(arguments[0] as u64)
            .map_err(|_| STATUS_INVALID_ROOT)?;
        let bytes = heap
            .heap
            .bytes(reference)
            .map_err(|_| STATUS_INVALID_ROOT)?;
        if operation == HEAP_BYTES_LEN {
            return Ok(bytes.len() as u64);
        }
        let index = usize::try_from(arguments[1])
            .ok()
            .filter(|index| {
                index
                    .checked_add(width)
                    .is_some_and(|end| end <= bytes.len())
            })
            .ok_or(STATUS_INVALID_BYTES)?;
        if operation == HEAP_BYTE_AT {
            return Ok(u64::from(bytes[index]));
        }
        if operation == HEAP_READ_I64_BE {
            let value = bytes[index..index + 8]
                .try_into()
                .expect("eight byte range");
            return Ok(i64::from_be_bytes(value) as u64);
        }
        let value = if operation == HEAP_REPLACE_I64_BE {
            arguments[2] as u64
        } else {
            u64::from(u8::try_from(arguments[2]).map_err(|_| STATUS_INVALID_BYTES)?)
        };
        (bytes.len(), Some(reference), index, value)
    };
    let mut buffer = match unsafe { &*pointer }.heap.prepare_bytes(length) {
        Ok(buffer) => buffer,
        Err(_) => {
            unsafe { collect_managed(context) }?;
            unsafe { &*pointer }
                .heap
                .prepare_bytes(length)
                .map_err(|_| STATUS_HEAP_EXHAUSTED)?
        }
    };
    for start in (0..length).step_by(256) {
        crate::execution::runtime_checkpoint(context, true)?;
        let end = (start + 256).min(length);
        if let Some(source) = source {
            let heap = unsafe { &*pointer };
            let bytes = heap.heap.bytes(source).map_err(|_| STATUS_INVALID_ROOT)?;
            buffer.data.extend_from_slice(&bytes[start..end]);
        } else {
            buffer.data.resize(end, value as u8);
        }
    }
    if source.is_some() {
        if width == 8 {
            buffer.data[index..index + 8].copy_from_slice(&value.to_be_bytes());
        } else {
            buffer.data[index] = value as u8;
        }
    }
    let mut buffer = Some(buffer);
    for attempt in 0..2 {
        let mut cursor = 0;
        loop {
            crate::execution::runtime_checkpoint(context, true)?;
            let result = unsafe { &mut *pointer }
                .heap
                .allocate_bytes_chunk(&mut buffer, &mut cursor);
            match result {
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

pub(crate) unsafe extern "C" fn heap_call(
    context: *mut ScalarContext,
    operation: u32,
    argument: u64,
    output: *mut u64,
) -> u32 {
    let context = unsafe { &mut *context };
    if context.heap_data.is_null() {
        return STATUS_INVALID_CONTROL;
    }
    let pointer = context.heap_data.cast::<AotHeap>();
    if (wings_abi::HEAP_FILL_BYTES..=wings_abi::HEAP_REPLACE_I64_BE).contains(&operation) {
        let arguments = unsafe { *(argument as usize as *const [i64; 3]) };
        return match unsafe { byte_call(context, operation, arguments) } {
            Ok(value) => {
                unsafe { *output = value };
                STATUS_OK
            }
            Err(status) => status,
        };
    }
    if operation == HEAP_COLLECT {
        if let Err(status) = unsafe { collect_managed(context) } {
            return status;
        }
        unsafe {
            *output = 0;
        }
        return STATUS_OK;
    }
    if matches!(
        operation,
        HEAP_RECORD_INT | HEAP_EMPTY_INTS | HEAP_PREPEND_INT
    ) {
        let (layout, values, count) = {
            let heap = unsafe { &*pointer };
            match operation {
                HEAP_RECORD_INT => (
                    heap.layout,
                    [RecordValue::Int(argument as i64), RecordValue::Unit],
                    1,
                ),
                HEAP_EMPTY_INTS => (heap.empty_list, [RecordValue::Unit; 2], 1),
                _ => {
                    let Ok(tail) = heap.list_reference(unsafe { *output }) else {
                        return STATUS_INVALID_ROOT;
                    };
                    (
                        heap.cons_list,
                        [RecordValue::Int(argument as i64), RecordValue::Ref(tail)],
                        2,
                    )
                }
            }
        };
        match unsafe { allocate_managed(context, layout, &values[..count]) } {
            Ok(value) => {
                unsafe {
                    *output = value;
                }
                return STATUS_OK;
            }
            Err(status) => return status,
        }
    }
    let heap = unsafe { &mut *pointer };
    let value = match operation {
        HEAP_HEAD_INT | HEAP_TAIL_INTS | HEAP_IS_EMPTY_INTS => {
            let Ok(reference) = heap.list_reference(argument) else {
                return STATUS_INVALID_ROOT;
            };
            let empty = heap
                .heap
                .has_layout(reference, heap.empty_list)
                .unwrap_or(false);
            if operation == HEAP_IS_EMPTY_INTS {
                u64::from(empty)
            } else if empty {
                return STATUS_EMPTY_LIST;
            } else if operation == HEAP_HEAD_INT {
                let Ok(RecordValue::Int(value)) = heap.heap.get(reference, 0) else {
                    return STATUS_INVALID_ROOT;
                };
                value as u64
            } else {
                let Ok(RecordValue::Ref(tail)) = heap.heap.get(reference, 1) else {
                    return STATUS_INVALID_ROOT;
                };
                tail.encoded()
            }
        }
        HEAP_READ_INT => {
            let Ok(reference) = heap.heap.import_reference(argument) else {
                return STATUS_INVALID_ROOT;
            };
            if !heap
                .heap
                .has_layout(reference, heap.layout)
                .unwrap_or(false)
            {
                return STATUS_INVALID_ROOT;
            }
            let Ok(RecordValue::Int(value)) = heap.heap.get(reference, 0) else {
                return STATUS_INVALID_ROOT;
            };
            value as u64
        }
        _ => return STATUS_INVALID_CONTROL,
    };
    unsafe { *output = value };
    STATUS_OK
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Resources;

    fn fixture(capacity: usize) -> (ResourceAccount, Box<AotHeap>) {
        let limits = Resources {
            bytes: 1 << 20,
            ..Resources::default()
        };
        let vm = ResourceAccount::vm(limits);
        let tenant = vm.tenant(limits).unwrap();
        let account = tenant.process(limits).unwrap();
        (vm, AotHeap::new(&account, capacity).unwrap())
    }

    fn context(heap: &mut AotHeap, flag: &std::sync::atomic::AtomicU32) -> ScalarContext {
        ScalarContext {
            cancellation: flag,
            remaining_steps: 4096,
            yield_request: flag,
            slice_remaining: 1,
            slow_path: Some(collect_on_resume),
            data: std::ptr::null_mut(),
            stack_limit: 0,
            roots: std::ptr::null(),
            heap_call: None,
            heap_data: std::ptr::from_mut(heap).cast(),
            effect_call: None,
        }
    }

    unsafe extern "C" fn collect_on_resume(pointer: *mut ScalarContext) -> u32 {
        let context = unsafe { &mut *pointer };
        let heap = unsafe { &mut *context.heap_data.cast::<AotHeap>() };
        if unsafe { heap.collect(context.roots) }.is_err() {
            return STATUS_INVALID_ROOT;
        }
        context.slice_remaining = 1;
        STATUS_OK
    }

    unsafe extern "C" fn resume_one_step(pointer: *mut ScalarContext) -> u32 {
        unsafe { (*pointer).slice_remaining = 1 };
        STATUS_OK
    }

    #[test]
    fn byte_exports_are_independent_accounted_and_bounded_before_publication() {
        let (vm, mut heap) = fixture(4);
        let flag = std::sync::atomic::AtomicU32::new(0);
        let mut context = context(&mut heap, &flag);
        let (status, source) =
            unsafe { bytes(&mut context, wings_abi::HEAP_FILL_BYTES, [65536, 201, 0]) };
        assert_eq!(status, STATUS_OK);
        heap.commit_typed(wings_abi::ValueType::BytesRef, source as i64)
            .unwrap();
        let baseline = vm.used();
        let mut arguments = [source as i64];
        let payload = unsafe { export_bytes(&mut context, &mut arguments, 1) }.unwrap();
        assert!(payload.valid(&arguments, 1));
        assert_eq!(payload.data, vec![201; MAX_MESSAGE_BYTES]);
        assert_eq!(vm.used().bytes, baseline.bytes + MAX_MESSAGE_BYTES as u64);
        let mut arguments = [source as i64, source as i64];
        assert!(matches!(
            unsafe { export_bytes(&mut context, &mut arguments, 3) },
            Err(wings_abi::STATUS_SEND_REJECTED)
        ));
        assert_eq!(vm.used().bytes, baseline.bytes + MAX_MESSAGE_BYTES as u64);
        for steps in [0, 2, 32, 128, 256] {
            context.remaining_steps = steps;
            let mut arguments = [source as i64];
            assert!(matches!(
                unsafe { export_bytes(&mut context, &mut arguments, 1) },
                Err(wings_abi::STATUS_BUDGET_EXHAUSTED)
            ));
            assert_eq!(vm.used().bytes, baseline.bytes + MAX_MESSAGE_BYTES as u64);
        }
        context.remaining_steps = 4096;
        flag.store(1, std::sync::atomic::Ordering::SeqCst);
        assert!(matches!(
            unsafe { export_bytes(&mut context, &mut [source as i64], 1) },
            Err(wings_abi::STATUS_CANCELLED)
        ));
        flag.store(0, std::sync::atomic::Ordering::SeqCst);
        heap.committed = None;
        unsafe { heap.collect(std::ptr::null()) }.unwrap();
        assert_eq!(heap.stats().allocated, 0);
        assert_eq!(payload.data, vec![201; MAX_MESSAGE_BYTES]);
        drop(heap);
        assert_eq!(vm.used().bytes, MAX_MESSAGE_BYTES as u64);
        drop(payload);
        assert_eq!(vm.used(), Resources::default());
    }

    #[test]
    fn byte_imports_root_mixed_arguments_at_every_copy_and_collection_checkpoint() {
        let (vm, mut heap) = fixture(7);
        for _ in 0..7 {
            heap.heap
                .allocate(heap.layout, &[RecordValue::Int(0)])
                .unwrap();
        }
        let flag = std::sync::atomic::AtomicU32::new(0);
        let mut context = context(&mut heap, &flag);
        let mut bytes = BytePayload::new(&heap.account, 0b11001, MAX_MESSAGE_BYTES).unwrap();
        bytes
            .data
            .extend((0..MAX_MESSAGE_BYTES).map(|index| (index % 251) as u8));
        let lists = ListPayload {
            mask: 4,
            count: 2,
            values: std::array::from_fn(|index| if index == 0 { i64::MIN } else { i64::MAX }),
        };
        let mut arguments = [
            ListPayload::encode(0, 32768),
            17,
            ListPayload::encode(0, 2),
            ListPayload::encode(32768, 32768),
            ListPayload::encode(65536, 0),
        ];
        unsafe { import_arguments(&mut context, &mut arguments, 2, &lists, &bytes) }.unwrap();
        unsafe { heap.collect(std::ptr::null()) }.unwrap();
        assert_eq!(heap.stats().allocated, 7);
        assert_eq!(heap.record_value(arguments[1]), Ok(17));
        for (index, expected) in [
            (0, &bytes.data[..32768]),
            (3, &bytes.data[32768..]),
            (4, &bytes.data[0..0]),
        ] {
            let reference = heap.heap.import_reference(arguments[index] as u64).unwrap();
            assert_eq!(heap.heap.bytes(reference).unwrap(), expected);
        }
        let Some((first, tail)) = heap.list_node(arguments[2]).unwrap() else {
            panic!("missing imported list")
        };
        assert_eq!(first, i64::MIN);
        assert_eq!(heap.list_node(tail).unwrap().unwrap().0, i64::MAX);
        heap.clear_incoming();
        unsafe { heap.collect(std::ptr::null()) }.unwrap();
        assert_eq!(heap.stats().allocated, 0);
        assert_eq!(bytes.data.len(), MAX_MESSAGE_BYTES);
        drop((heap, bytes));
        assert_eq!(vm.used(), Resources::default());
    }

    unsafe fn bytes(
        context: &mut ScalarContext,
        operation: u32,
        arguments: [i64; 3],
    ) -> (u32, u64) {
        let mut output = 79;
        let status = unsafe {
            heap_call(
                context,
                operation,
                arguments.as_ptr() as usize as u64,
                &mut output,
            )
        };
        (status, output)
    }

    #[test]
    fn byte_calls_keep_source_live_through_every_copy_checkpoint_and_validate_shape() {
        use wings_abi::{
            HEAP_BYTE_AT, HEAP_BYTES_LEN, HEAP_FILL_BYTES, HEAP_REPLACE_BYTE, STATUS_INVALID_BYTES,
        };
        let (vm, mut heap) = fixture(4);
        let flag = std::sync::atomic::AtomicU32::new(0);
        let mut context = context(&mut heap, &flag);
        let (status, original) = unsafe { bytes(&mut context, HEAP_FILL_BYTES, [65536, 255, 0]) };
        assert_eq!(status, STATUS_OK);
        heap.commit_typed(wings_abi::ValueType::BytesRef, original as i64)
            .unwrap();
        let (status, replaced) =
            unsafe { bytes(&mut context, HEAP_REPLACE_BYTE, [original as i64, 32768, 7]) };
        assert_eq!(status, STATUS_OK);
        assert!(heap.stats().collections > 500);
        let slots = [replaced];
        let frame = ShadowFrame {
            previous: std::ptr::null(),
            slots: slots.as_ptr(),
            count: 1,
        };
        context.roots = &frame;
        assert_eq!(
            unsafe { bytes(&mut context, HEAP_BYTES_LEN, [replaced as i64, 0, 0]) },
            (STATUS_OK, 65536)
        );
        assert_eq!(
            unsafe { bytes(&mut context, HEAP_BYTE_AT, [replaced as i64, 32768, 0]) },
            (STATUS_OK, 7)
        );
        assert_eq!(
            unsafe { bytes(&mut context, HEAP_BYTE_AT, [original as i64, 32768, 0]) },
            (STATUS_OK, 255)
        );
        let baseline = vm.used();
        for (operation, arguments) in [
            (HEAP_FILL_BYTES, [-1, 0, 0]),
            (HEAP_FILL_BYTES, [65537, 0, 0]),
            (HEAP_FILL_BYTES, [0, -1, 0]),
            (HEAP_FILL_BYTES, [1, 256, 0]),
            (HEAP_BYTE_AT, [original as i64, -1, 0]),
            (HEAP_BYTE_AT, [original as i64, 65536, 0]),
            (HEAP_REPLACE_BYTE, [original as i64, 0, -1]),
            (HEAP_REPLACE_BYTE, [original as i64, 0, 256]),
        ] {
            assert_eq!(
                unsafe { bytes(&mut context, operation, arguments) },
                (STATUS_INVALID_BYTES, 79)
            );
            assert_eq!(vm.used(), baseline);
        }
        assert!(heap.commit(original as i64).is_err());
        assert!(heap.commit_list(original as i64).is_err());
        let record = heap
            .heap
            .allocate(heap.layout, &[RecordValue::Int(3)])
            .unwrap();
        assert_eq!(
            unsafe {
                bytes(
                    &mut context,
                    HEAP_BYTES_LEN,
                    [record.encoded() as i64, 0, 0],
                )
            },
            (STATUS_INVALID_ROOT, 79)
        );
        assert!(
            heap.commit_typed(wings_abi::ValueType::BytesRef, record.encoded() as i64)
                .is_err()
        );
        drop(heap);
        assert_eq!(vm.used(), Resources::default());
    }

    #[test]
    fn byte_builder_budget_and_cancellation_release_unpublished_data_at_every_chunk() {
        use wings_abi::{
            HEAP_FILL_BYTES, HEAP_REPLACE_BYTE, HEAP_REPLACE_I64_BE, STATUS_BUDGET_EXHAUSTED,
            STATUS_CANCELLED,
        };
        for budget in [0, 1, 2, 16, 64, 128, 255, 256] {
            let (vm, mut heap) = fixture(4);
            let flag = std::sync::atomic::AtomicU32::new(0);
            let mut context = context(&mut heap, &flag);
            context.slow_path = Some(resume_one_step);
            let (_, original) = unsafe { bytes(&mut context, HEAP_FILL_BYTES, [65536, 9, 0]) };
            heap.commit_typed(wings_abi::ValueType::BytesRef, original as i64)
                .unwrap();
            let baseline = vm.used();
            for operation in [HEAP_FILL_BYTES, HEAP_REPLACE_BYTE, HEAP_REPLACE_I64_BE] {
                context.remaining_steps = budget;
                context.slice_remaining = 1;
                let arguments = if operation == HEAP_FILL_BYTES {
                    [65536, 7, 0]
                } else {
                    [original as i64, 42, 7]
                };
                assert_eq!(
                    unsafe { bytes(&mut context, operation, arguments) },
                    (STATUS_BUDGET_EXHAUSTED, 79)
                );
                assert_eq!(vm.used(), baseline);
                assert_eq!(heap.stats().allocated, 1);
            }
            flag.store(1, std::sync::atomic::Ordering::SeqCst);
            context.remaining_steps = 4096;
            assert_eq!(
                unsafe { bytes(&mut context, HEAP_FILL_BYTES, [65536, 1, 0]) },
                (STATUS_CANCELLED, 79)
            );
            assert_eq!(vm.used(), baseline);
            drop(heap);
            assert_eq!(vm.used(), Resources::default());
        }
    }

    #[test]
    fn byte_i64_access_is_big_endian_signed_unaligned_immutable_and_rooted() {
        use wings_abi::{HEAP_FILL_BYTES, HEAP_READ_I64_BE, HEAP_REPLACE_I64_BE};
        for value in [i64::MIN, -65537, -1, 0, 1, 65537, i64::MAX] {
            for offset in [0, 1, 8] {
                let (vm, mut heap) = fixture(4);
                let flag = std::sync::atomic::AtomicU32::new(0);
                let mut context = context(&mut heap, &flag);
                let (status, original) =
                    unsafe { bytes(&mut context, HEAP_FILL_BYTES, [16, 0, 0]) };
                assert_eq!(status, STATUS_OK);
                heap.commit_typed(wings_abi::ValueType::BytesRef, original as i64)
                    .unwrap();
                let (status, replacement) = unsafe {
                    bytes(
                        &mut context,
                        HEAP_REPLACE_I64_BE,
                        [original as i64, offset, value],
                    )
                };
                assert_eq!(status, STATUS_OK);
                let original_ref = heap.heap.import_reference(original).unwrap();
                let replacement_ref = heap.heap.import_reference(replacement).unwrap();
                assert_eq!(heap.heap.bytes(original_ref).unwrap(), [0; 16]);
                let mut expected = [0; 16];
                expected[offset as usize..offset as usize + 8]
                    .copy_from_slice(&value.to_be_bytes());
                assert_eq!(heap.heap.bytes(replacement_ref).unwrap(), expected);
                assert_eq!(
                    unsafe {
                        bytes(
                            &mut context,
                            HEAP_READ_I64_BE,
                            [replacement as i64, offset, 0],
                        )
                    },
                    (STATUS_OK, value as u64)
                );
                heap.commit_typed(wings_abi::ValueType::BytesRef, replacement as i64)
                    .unwrap();
                unsafe { heap.collect(std::ptr::null()) }.unwrap();
                assert_eq!(heap.stats().allocated, 1);
                assert_eq!(heap.heap.bytes(replacement_ref).unwrap(), expected);
                drop(heap);
                assert_eq!(vm.used(), Resources::default());
            }
        }
    }

    #[test]
    fn byte_i64_range_rejections_do_not_allocate_or_change_output() {
        use wings_abi::{
            HEAP_FILL_BYTES, HEAP_READ_I64_BE, HEAP_REPLACE_I64_BE, STATUS_INVALID_BYTES,
        };
        for length in [0, 1, 7, 8, 16] {
            let (vm, mut heap) = fixture(4);
            let flag = std::sync::atomic::AtomicU32::new(0);
            let mut context = context(&mut heap, &flag);
            let (status, original) =
                unsafe { bytes(&mut context, HEAP_FILL_BYTES, [length, 255, 0]) };
            assert_eq!(status, STATUS_OK);
            heap.commit_typed(wings_abi::ValueType::BytesRef, original as i64)
                .unwrap();
            let baseline = vm.used();
            for offset in [i64::MIN, -1, length.saturating_sub(7), length, i64::MAX] {
                for operation in [HEAP_READ_I64_BE, HEAP_REPLACE_I64_BE] {
                    assert_eq!(
                        unsafe {
                            bytes(&mut context, operation, [original as i64, offset, i64::MIN])
                        },
                        (STATUS_INVALID_BYTES, 79)
                    );
                    assert_eq!(vm.used(), baseline);
                    assert_eq!(heap.stats().allocated, 1);
                }
            }
            drop(heap);
            assert_eq!(vm.used(), Resources::default());
        }
    }

    #[test]
    fn zero_bytes_and_slot_gc_retry_keep_control_and_data_charges_separate() {
        use wings_abi::{HEAP_BYTE_AT, HEAP_BYTES_LEN, HEAP_FILL_BYTES, STATUS_INVALID_BYTES};
        let (vm, mut heap) = fixture(1);
        let flag = std::sync::atomic::AtomicU32::new(0);
        let mut context = context(&mut heap, &flag);
        context.slow_path = Some(resume_one_step);
        let baseline = vm.used();
        let (status, empty) = unsafe { bytes(&mut context, HEAP_FILL_BYTES, [0, 0, 0]) };
        assert_eq!(status, STATUS_OK);
        assert_eq!(
            unsafe { bytes(&mut context, HEAP_BYTES_LEN, [empty as i64, 0, 0]) },
            (STATUS_OK, 0)
        );
        assert_eq!(
            unsafe { bytes(&mut context, HEAP_BYTE_AT, [empty as i64, 0, 0]) },
            (STATUS_INVALID_BYTES, 79)
        );
        assert_eq!(vm.used(), baseline);
        let (status, next) = unsafe { bytes(&mut context, HEAP_FILL_BYTES, [512, 9, 0]) };
        assert_eq!(status, STATUS_OK);
        assert_ne!(empty, next);
        assert_eq!(heap.stats().allocated, 1);
        assert_eq!(heap.stats().collections, 1);
        assert_eq!(vm.used().bytes, baseline.bytes + 512);
        drop(heap);
        assert_eq!(vm.used(), Resources::default());
    }

    #[test]
    fn byte_quota_retry_reclaims_only_unreachable_data_and_rejects_live_overflow() {
        use wings_abi::{HEAP_FILL_BYTES, HEAP_REPLACE_BYTE};
        let (vm, mut heap) = fixture(4);
        let flag = std::sync::atomic::AtomicU32::new(0);
        let mut context = context(&mut heap, &flag);
        context.slow_path = Some(resume_one_step);
        let baseline = vm.used();
        let guard = vm
            .reserve(Resources {
                bytes: (1 << 20) - baseline.bytes - 512,
                ..Resources::default()
            })
            .unwrap();
        let (status, stale) = unsafe { bytes(&mut context, HEAP_FILL_BYTES, [512, 1, 0]) };
        assert_eq!(status, STATUS_OK);
        let (status, current) = unsafe { bytes(&mut context, HEAP_FILL_BYTES, [512, 2, 0]) };
        assert_eq!(status, STATUS_OK);
        assert_ne!(stale, current);
        assert_eq!(heap.stats().collections, 1);
        assert_eq!(heap.stats().allocated, 1);
        heap.commit_typed(wings_abi::ValueType::BytesRef, current as i64)
            .unwrap();
        let full = vm.used();
        assert_eq!(
            unsafe { bytes(&mut context, HEAP_REPLACE_BYTE, [current as i64, 0, 3]) },
            (STATUS_HEAP_EXHAUSTED, 79)
        );
        assert_eq!(vm.used(), full);
        assert_eq!(heap.stats().collections, 2);
        assert_eq!(heap.stats().allocated, 1);
        let reference = heap.heap.import_reference(current).unwrap();
        assert_eq!(heap.heap.bytes(reference).unwrap(), [2; 512]);
        drop(guard);
        drop(heap);
        assert_eq!(vm.used(), Resources::default());
    }

    #[test]
    fn list_exports_poll_each_node_keep_source_roots_and_reject_stale_layouts() {
        let (vm, mut heap) = fixture(68);
        let mut tail = heap
            .heap
            .allocate(heap.empty_list, &[RecordValue::Unit])
            .unwrap();
        for value in 0..65 {
            tail = heap
                .heap
                .allocate(
                    heap.cons_list,
                    &[RecordValue::Int(value), RecordValue::Ref(tail)],
                )
                .unwrap();
        }
        let nodes = [tail.encoded()];
        let frame = ShadowFrame {
            previous: std::ptr::null(),
            slots: nodes.as_ptr(),
            count: 1,
        };
        let flag = std::sync::atomic::AtomicU32::new(0);
        let mut context = context(&mut heap, &flag);
        context.roots = &frame;
        let mut arguments = [tail.encoded() as i64];
        assert!(matches!(
            unsafe { export_lists(&mut context, &mut arguments, 1) },
            Err(wings_abi::STATUS_SEND_REJECTED)
        ));
        assert_eq!(arguments[0], tail.encoded() as i64);
        let Some((_, shorter)) = heap.list_node(tail.encoded() as i64).unwrap() else {
            panic!("expected nonempty list")
        };
        let mut arguments = [shorter];
        context.remaining_steps = 65;
        let payload = unsafe { export_lists(&mut context, &mut arguments, 1) }.unwrap();
        assert!(payload.valid(&arguments, 1));
        assert_eq!(context.remaining_steps, 0);
        assert_eq!(payload.count, 64);
        assert_eq!(
            payload.values,
            std::array::from_fn(|index| 63 - index as i64)
        );
        let mut arguments = [shorter];
        context.remaining_steps = 2;
        assert!(matches!(
            unsafe { export_lists(&mut context, &mut arguments, 1) },
            Err(wings_abi::STATUS_BUDGET_EXHAUSTED)
        ));
        flag.store(1, std::sync::atomic::Ordering::SeqCst);
        assert!(matches!(
            unsafe { export_lists(&mut context, &mut arguments, 1) },
            Err(wings_abi::STATUS_CANCELLED)
        ));
        flag.store(0, std::sync::atomic::Ordering::SeqCst);
        context.remaining_steps = 4096;
        let record = heap
            .heap
            .allocate(heap.layout, &[RecordValue::Int(9)])
            .unwrap();
        let mut arguments = [record.encoded() as i64];
        context.slice_remaining = 1;
        assert!(matches!(
            unsafe { export_lists(&mut context, &mut arguments, 1) },
            Err(STATUS_INVALID_ROOT)
        ));
        context.roots = std::ptr::null();
        unsafe { heap.collect(std::ptr::null()) }.unwrap();
        let mut arguments = [shorter];
        assert!(matches!(
            unsafe { export_lists(&mut context, &mut arguments, 1) },
            Err(STATUS_INVALID_ROOT)
        ));
        drop(heap);
        assert_eq!(vm.used(), Resources::default());
    }

    #[test]
    fn list_imports_root_each_tail_and_prior_mixed_arguments_at_every_checkpoint() {
        let (vm, mut heap) = fixture(9);
        for _ in 0..9 {
            heap.heap
                .allocate(heap.layout, &[RecordValue::Int(0)])
                .unwrap();
        }
        let flag = std::sync::atomic::AtomicU32::new(0);
        let mut context = context(&mut heap, &flag);
        context.slice_remaining = 0;
        let payload = ListPayload {
            mask: 0b10101,
            count: 4,
            values: std::array::from_fn(|index| [i64::MIN, i64::MAX, -7, 9][index % 4]),
        };
        let mut arguments = [
            ListPayload::encode(0, 2),
            17,
            ListPayload::encode(2, 2),
            1,
            ListPayload::encode(4, 0),
        ];
        unsafe {
            import_arguments(
                &mut context,
                &mut arguments,
                2,
                &payload,
                &BytePayload::default(),
            )
        }
        .unwrap();
        unsafe { heap.collect(std::ptr::null()) }.unwrap();
        assert_eq!(heap.record_value(arguments[1]), Ok(17));
        assert_eq!(heap.stats().allocated, 8);
        for (index, expected) in [(0, vec![i64::MIN, i64::MAX]), (2, vec![-7, 9]), (4, vec![])] {
            let mut current = arguments[index];
            for value in expected {
                let Some((actual, tail)) = heap.list_node(current).unwrap() else {
                    panic!("short list")
                };
                assert_eq!(actual, value);
                current = tail;
            }
            assert_eq!(heap.list_node(current).unwrap(), None);
        }
        heap.clear_incoming();
        unsafe { heap.collect(std::ptr::null()) }.unwrap();
        assert_eq!(heap.stats().allocated, 0);
        let mut arguments = [0; 64];
        let empty_payload = ListPayload {
            mask: u64::MAX,
            ..ListPayload::default()
        };
        context.remaining_steps = 4096;
        context.slow_path = Some(resume_one_step);
        assert_eq!(
            unsafe {
                import_arguments(
                    &mut context,
                    &mut arguments,
                    0,
                    &empty_payload,
                    &BytePayload::default(),
                )
            },
            Err(STATUS_HEAP_EXHAUSTED)
        );
        assert_eq!(heap.stats().allocated, 9);
        heap.clear_incoming();
        unsafe { heap.collect(std::ptr::null()) }.unwrap();
        drop(heap);
        assert_eq!(vm.used(), Resources::default());
    }

    #[test]
    fn all_64_empty_list_parameters_have_independent_incoming_roots() {
        let (vm, mut heap) = fixture(64);
        let flag = std::sync::atomic::AtomicU32::new(0);
        let mut context = context(&mut heap, &flag);
        let mut arguments = [0; 64];
        let payload = ListPayload {
            mask: u64::MAX,
            ..ListPayload::default()
        };
        unsafe {
            import_arguments(
                &mut context,
                &mut arguments,
                0,
                &payload,
                &BytePayload::default(),
            )
        }
        .unwrap();
        unsafe { heap.collect(std::ptr::null()) }.unwrap();
        assert_eq!(heap.stats().allocated, 64);
        for argument in arguments {
            assert_eq!(heap.list_node(argument).unwrap(), None);
        }
        heap.clear_incoming();
        unsafe { heap.collect(std::ptr::null()) }.unwrap();
        assert_eq!(heap.stats().allocated, 0);
        drop(heap);
        assert_eq!(vm.used(), Resources::default());
    }

    #[test]
    fn imports_all_64_record_parameters_including_the_highest_mask_bit() {
        let (vm, mut heap) = fixture(64);
        let cancellation = std::sync::atomic::AtomicU32::new(0);
        let mut context = ScalarContext {
            cancellation: &cancellation,
            remaining_steps: 256,
            yield_request: &cancellation,
            slice_remaining: 256,
            slow_path: None,
            data: std::ptr::null_mut(),
            stack_limit: 0,
            roots: std::ptr::null(),
            heap_call: None,
            heap_data: std::ptr::from_mut(&mut *heap).cast(),
            effect_call: None,
        };
        let mut arguments = std::array::from_fn::<_, 64, _>(|index| index as i64 - 32);
        unsafe {
            import_arguments(
                &mut context,
                &mut arguments,
                u64::MAX,
                &ListPayload::default(),
                &BytePayload::default(),
            )
        }
        .unwrap();
        unsafe { heap.collect(std::ptr::null()) }.unwrap();
        assert_eq!(heap.stats().allocated, 64);
        assert_eq!(context.remaining_steps, 192);
        for (index, argument) in arguments.iter().enumerate() {
            assert_eq!(heap.record_value(*argument), Ok(index as i64 - 32));
        }
        heap.clear_incoming();
        unsafe { heap.collect(std::ptr::null()) }.unwrap();
        assert_eq!(heap.stats().allocated, 0);
        drop(heap);
        assert_eq!(vm.used(), Resources::default());
    }

    #[test]
    fn incoming_parameters_survive_gc_before_entry_and_later_parameter_allocation() {
        let (vm, mut heap) = fixture(3);
        let committed = heap
            .heap
            .allocate(heap.layout, &[RecordValue::Int(7)])
            .unwrap();
        heap.commit(committed.encoded() as i64).unwrap();
        heap.heap
            .allocate(heap.layout, &[RecordValue::Int(8)])
            .unwrap();
        heap.heap
            .allocate(heap.layout, &[RecordValue::Int(9)])
            .unwrap();
        let cancellation = std::sync::atomic::AtomicU32::new(0);
        let yielding = std::sync::atomic::AtomicU32::new(0);
        let mut context = ScalarContext {
            cancellation: &cancellation,
            remaining_steps: 128,
            yield_request: &yielding,
            slice_remaining: 128,
            slow_path: None,
            data: std::ptr::null_mut(),
            stack_limit: 0,
            roots: std::ptr::null(),
            heap_call: None,
            heap_data: std::ptr::from_mut(&mut *heap).cast(),
            effect_call: None,
        };
        let mut arguments = [11, 22];
        unsafe {
            import_arguments(
                &mut context,
                &mut arguments,
                3,
                &ListPayload::default(),
                &BytePayload::default(),
            )
        }
        .unwrap();
        assert_eq!(heap.record_value(arguments[0]), Ok(11));
        assert_eq!(heap.record_value(arguments[1]), Ok(22));
        let first = heap.heap.import_reference(arguments[0] as u64).unwrap();
        let second = heap.heap.import_reference(arguments[1] as u64).unwrap();
        heap.incoming =
            std::array::from_fn(|index| Some(if index % 2 == 0 { first } else { second }));
        let frame_slots = [arguments[1] as u64; MAX_SHADOW_ROOTS];
        let frame = ShadowFrame {
            previous: std::ptr::null(),
            slots: frame_slots.as_ptr(),
            count: frame_slots.len(),
        };
        unsafe { heap.collect(&frame) }.unwrap();
        assert_eq!(heap.stats().allocated, 3);
        assert_eq!(heap.record_value(arguments[0]), Ok(11));
        heap.clear_incoming();
        unsafe { heap.collect(std::ptr::null()) }.unwrap();
        assert_eq!(heap.stats().allocated, 1);
        assert_eq!(heap.record_value(arguments[0]), Err(STATUS_INVALID_ROOT));
        let list = heap
            .heap
            .allocate(heap.empty_list, &[RecordValue::Unit])
            .unwrap();
        assert_eq!(
            heap.record_value(list.encoded() as i64),
            Err(STATUS_INVALID_ROOT)
        );
        drop(heap);
        assert_eq!(vm.used(), Resources::default());
    }

    #[test]
    fn typed_state_roots_reject_other_shapes_and_trace_list_tails() {
        let (vm, mut heap) = fixture(5);
        let empty = heap
            .heap
            .allocate(heap.empty_list, &[RecordValue::Unit])
            .unwrap();
        let tail = heap
            .heap
            .allocate(
                heap.cons_list,
                &[RecordValue::Int(7), RecordValue::Ref(empty)],
            )
            .unwrap();
        let head = heap
            .heap
            .allocate(
                heap.cons_list,
                &[RecordValue::Int(11), RecordValue::Ref(tail)],
            )
            .unwrap();
        let record = heap
            .heap
            .allocate(heap.layout, &[RecordValue::Int(99)])
            .unwrap();
        assert!(heap.commit_list(record.encoded() as i64).is_err());
        assert!(heap.commit(head.encoded() as i64).is_err());
        heap.commit_list(head.encoded() as i64).unwrap();
        let used = vm.used();
        unsafe { heap.collect(std::ptr::null()) }.unwrap();
        assert_eq!(heap.stats().allocated, 3);
        assert!(heap.heap.get(record, 0).is_err());
        assert_eq!(heap.heap.get(tail, 0).unwrap(), RecordValue::Int(7));
        heap.commit_list(empty.encoded() as i64).unwrap();
        unsafe { heap.collect(std::ptr::null()) }.unwrap();
        assert_eq!(heap.stats().allocated, 1);
        assert!(heap.commit_list(head.encoded() as i64).is_err());
        assert_eq!(vm.used(), used);
        drop(heap);
        assert_eq!(vm.used(), Resources::default());
    }

    #[test]
    fn committed_root_is_independent_from_sixty_four_compiler_slots() {
        let (vm, mut heap) = fixture(2);
        let committed = heap
            .heap
            .allocate(heap.layout, &[RecordValue::Int(7)])
            .unwrap();
        let candidate = heap
            .heap
            .allocate(heap.layout, &[RecordValue::Int(9)])
            .unwrap();
        heap.commit(committed.encoded() as i64).unwrap();
        let slots = [candidate.encoded(); MAX_SHADOW_ROOTS];
        let frame = ShadowFrame {
            previous: std::ptr::null(),
            slots: slots.as_ptr(),
            count: slots.len(),
        };
        unsafe { heap.collect(&frame) }.unwrap();
        assert_eq!(heap.stats().allocated, 2);
        let invalid_slots = [0];
        let invalid = ShadowFrame {
            previous: &frame,
            slots: invalid_slots.as_ptr(),
            count: 1,
        };
        let before = heap.stats();
        assert!(unsafe { heap.collect(&invalid) }.is_err());
        assert_eq!(heap.stats(), before);
        assert_eq!(heap.heap.get(committed, 0).unwrap(), RecordValue::Int(7));
        heap.commit(candidate.encoded() as i64).unwrap();
        unsafe { heap.collect(std::ptr::null()) }.unwrap();
        assert_eq!(heap.stats().allocated, 1);
        assert!(heap.heap.get(committed, 0).is_err());
        assert!(heap.commit(committed.encoded() as i64).is_err());
        assert_eq!(heap.committed, Some(candidate));
        assert!(heap.commit(0).is_err());
        unsafe { heap.collect(std::ptr::null()) }.unwrap();
        assert_eq!(heap.heap.get(candidate, 0).unwrap(), RecordValue::Int(9));
        drop(heap);
        assert_eq!(vm.used(), Resources::default());
    }

    #[test]
    fn nested_compiler_frames_keep_only_explicit_references() {
        let (vm, mut heap) = fixture(3);
        let first = heap
            .heap
            .allocate(heap.layout, &[RecordValue::Int(11)])
            .unwrap();
        let second = heap
            .heap
            .allocate(heap.layout, &[RecordValue::Int(22)])
            .unwrap();
        let dead = heap
            .heap
            .allocate(heap.layout, &[RecordValue::Int(33)])
            .unwrap();
        let parent_slots = [first.encoded()];
        let parent = ShadowFrame {
            previous: std::ptr::null(),
            slots: parent_slots.as_ptr(),
            count: 1,
        };
        let child_slots = [second.encoded(), first.encoded()];
        let child = ShadowFrame {
            previous: &parent,
            slots: child_slots.as_ptr(),
            count: 2,
        };
        let charged = vm.used();
        unsafe { heap.collect(&child) }.unwrap();
        assert_eq!(heap.stats().allocated, 2);
        assert!(heap.heap.get(dead, 0).is_err());
        unsafe { heap.collect(&parent) }.unwrap();
        assert_eq!(heap.stats().allocated, 1);
        assert_eq!(heap.heap.get(first, 0).unwrap(), RecordValue::Int(11));
        assert!(heap.heap.get(second, 0).is_err());
        unsafe { heap.collect(std::ptr::null()) }.unwrap();
        assert_eq!(heap.stats().allocated, 0);
        assert_eq!(vm.used(), charged);
        drop(heap);
        assert_eq!(vm.used(), Resources::default());
    }

    #[test]
    fn invalid_roots_and_frame_limits_never_start_collection() {
        let (_, mut heap) = fixture(2);
        let live = heap
            .heap
            .allocate(heap.layout, &[RecordValue::Int(1)])
            .unwrap();
        let slots = [live.encoded(), 0];
        let invalid_handle = ShadowFrame {
            previous: std::ptr::null(),
            slots: slots.as_ptr(),
            count: 2,
        };
        let null_slots = ShadowFrame {
            previous: std::ptr::null(),
            slots: std::ptr::null(),
            count: 1,
        };
        let oversized = ShadowFrame {
            previous: std::ptr::null(),
            slots: slots.as_ptr(),
            count: 65,
        };
        let mut cycle = ShadowFrame {
            previous: std::ptr::null(),
            slots: std::ptr::null(),
            count: 0,
        };
        cycle.previous = &cycle;
        for frame in [&invalid_handle, &null_slots, &oversized, &cycle] {
            let before = heap.stats();
            assert!(unsafe { heap.collect(frame) }.is_err());
            assert_eq!(heap.stats(), before);
            assert_eq!(heap.heap.get(live, 0).unwrap(), RecordValue::Int(1));
        }
        let slots = [live.encoded(); 64];
        let parent = ShadowFrame {
            previous: std::ptr::null(),
            slots: slots.as_ptr(),
            count: 64,
        };
        let child = ShadowFrame {
            previous: &parent,
            slots: slots.as_ptr(),
            count: 1,
        };
        let before = heap.stats();
        assert!(unsafe { heap.collect(&child) }.is_err());
        assert_eq!(heap.stats(), before);
        unsafe { heap.collect(&parent) }.unwrap();
        assert_eq!(heap.stats().allocated, 1);
    }

    #[test]
    fn stale_roots_fail_before_reclaiming_valid_objects() {
        let (_, mut heap) = fixture(1);
        let stale = heap
            .heap
            .allocate(heap.layout, &[RecordValue::Int(1)])
            .unwrap();
        unsafe { heap.collect(std::ptr::null()) }.unwrap();
        let current = heap
            .heap
            .allocate(heap.layout, &[RecordValue::Int(2)])
            .unwrap();
        let slots = [stale.encoded()];
        let frame = ShadowFrame {
            previous: std::ptr::null(),
            slots: slots.as_ptr(),
            count: 1,
        };
        let before = heap.stats();
        assert!(unsafe { heap.collect(&frame) }.is_err());
        assert_eq!(heap.stats(), before);
        assert_eq!(heap.heap.get(current, 0).unwrap(), RecordValue::Int(2));
    }
}
