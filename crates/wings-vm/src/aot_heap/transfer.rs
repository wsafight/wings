use super::*;

pub(crate) unsafe fn export_record_reply(
    context: &mut ScalarContext,
    encoded: i64,
    root: u16,
    records: &[wings_abi::RecordSchema],
) -> Result<RecordReply, u32> {
    let pointer = context.heap_data.cast::<AotHeap>();
    let heap = unsafe { pointer.as_ref() }.ok_or(STATUS_INVALID_ROOT)?;
    heap.typed_reference(wings_abi::ValueType::RecordRef(root), encoded)?;
    let account = heap.account.clone();
    let lease = match RecordReply::reserve(&account, root, records, Some(context)) {
        Ok(lease) => lease,
        Err(STATUS_HEAP_EXHAUSTED) => {
            unsafe { &mut *pointer }.heap.drop_spare_buffer();
            RecordReply::reserve(&account, root, records, Some(context))?
        }
        Err(err) => return Err(err),
    };
    let expansion =
        wings_abi::expand_record_schema(records, root).map_err(|_| STATUS_REPLY_REJECTED)?;
    let mut arguments = [0; 64];
    let mut references = [0; 64];
    let mut lists = ListPayload::default();
    let mut count = 0;
    collect_record_fields(
        context,
        encoded,
        root,
        records,
        &mut arguments,
        &mut references,
        &mut count,
    )?;
    if count != expansion.count {
        return Err(STATUS_INVALID_ROOT);
    }

    for (index, ty) in expansion.fields[..count].iter().enumerate() {
        if *ty != wings_abi::ValueType::ListRef {
            continue;
        }
        lists.mask |= 1u64 << index;
        let start = lists.len();
        let mut current = references[index] as i64;
        loop {
            crate::execution::runtime_checkpoint(context, true)?;
            let heap = unsafe { pointer.as_ref() }.ok_or(STATUS_INVALID_ROOT)?;
            let Some((value, tail)) = heap.list_node(current)? else {
                break;
            };
            lists
                .values
                .try_push(value)
                .map_err(|_| STATUS_REPLY_REJECTED)?;
            current = tail;
        }
        arguments[index] = ListPayload::encode(start, lists.len() - start);
    }

    let mut bytes_mask = 0;
    let mut string_mask = 0;
    let mut byte_count = 0usize;
    for (index, ty) in expansion.fields[..count].iter().enumerate() {
        if !matches!(
            ty,
            wings_abi::ValueType::BytesRef | wings_abi::ValueType::StringRef
        ) {
            continue;
        }
        crate::execution::runtime_checkpoint(context, true)?;
        let heap = unsafe { pointer.as_ref() }.ok_or(STATUS_INVALID_ROOT)?;
        let reference = heap.typed_reference(*ty, references[index] as i64)?;
        let is_string = *ty == wings_abi::ValueType::StringRef;
        let length = message_buffer(heap, reference, is_string)?.len();
        byte_count = byte_count
            .checked_add(length)
            .filter(|total| *total <= MAX_MESSAGE_BYTES)
            .ok_or(STATUS_REPLY_REJECTED)?;
        bytes_mask |= 1u64 << index;
        if is_string {
            string_mask |= 1u64 << index;
        }
    }
    let mut bytes = if bytes_mask == 0 {
        BytePayload::default()
    } else {
        let mut payload = match BytePayload::new(&account, bytes_mask, byte_count) {
            Ok(payload) => payload,
            Err(_) => {
                unsafe { &mut *pointer }.heap.drop_spare_buffer();
                BytePayload::new(&account, bytes_mask, byte_count)
                    .map_err(|_| STATUS_HEAP_EXHAUSTED)?
            }
        };
        payload.string_mask = string_mask;
        payload
    };
    for (index, ty) in expansion.fields[..count].iter().enumerate() {
        if bytes_mask & (1u64 << index) == 0 {
            continue;
        }
        let is_string = *ty == wings_abi::ValueType::StringRef;
        let heap = unsafe { pointer.as_ref() }.ok_or(STATUS_INVALID_ROOT)?;
        let reference = heap.typed_reference(*ty, references[index] as i64)?;
        let length = message_buffer(heap, reference, is_string)?.len();
        let start = bytes.data.len();
        for offset in (0..length).step_by(256) {
            crate::execution::runtime_checkpoint(context, true)?;
            let heap = unsafe { pointer.as_ref() }.ok_or(STATUS_INVALID_ROOT)?;
            let reference = heap.typed_reference(*ty, references[index] as i64)?;
            let source = message_buffer(heap, reference, is_string)?;
            bytes
                .data
                .extend_from_slice(&source[offset..(offset + 256).min(length)]);
        }
        arguments[index] = ListPayload::encode(start, length);
    }
    let payload = crate::host_values::HostMessage {
        arguments,
        count,
        lists,
        bytes,
    };
    RecordReply::from_parts(lease, payload).map_err(|_| STATUS_INVALID_ROOT)
}

#[allow(clippy::too_many_arguments)]
fn collect_record_fields(
    context: &mut ScalarContext,
    encoded: i64,
    schema: u16,
    records: &[wings_abi::RecordSchema],
    arguments: &mut [i64; 64],
    references: &mut [u64; 64],
    count: &mut usize,
) -> Result<(), u32> {
    crate::execution::runtime_checkpoint(context, true)?;
    let pointer = context.heap_data.cast::<AotHeap>();
    let heap = unsafe { pointer.as_ref() }.ok_or(STATUS_INVALID_ROOT)?;
    let reference = heap.typed_reference(wings_abi::ValueType::RecordRef(schema), encoded)?;
    let record = records
        .get(usize::from(schema))
        .ok_or(STATUS_INVALID_ROOT)?;
    heap.named
        .get(usize::from(schema))
        .filter(|layout| layout.count == record.fields.len())
        .ok_or(STATUS_INVALID_ROOT)?;
    for (field_index, (_, ty)) in record.fields.iter().enumerate() {
        crate::execution::runtime_checkpoint(context, true)?;
        let value = unsafe { pointer.as_ref() }
            .ok_or(STATUS_INVALID_ROOT)?
            .heap
            .get(reference, field_index)
            .map_err(|_| STATUS_INVALID_ROOT)?;
        if let wings_abi::ValueType::RecordRef(nested) = ty {
            let RecordValue::Ref(nested_value) = value else {
                return Err(STATUS_INVALID_ROOT);
            };
            collect_record_fields(
                context,
                nested_value.encoded() as i64,
                *nested,
                records,
                arguments,
                references,
                count,
            )?;
            continue;
        }
        if *count == wings_abi::MAX_RECORD_REPLY_NODES {
            return Err(STATUS_REPLY_REJECTED);
        }
        let index = *count;
        match (*ty, value) {
            (wings_abi::ValueType::Int, RecordValue::Int(value)) => arguments[index] = value,
            (wings_abi::ValueType::Pid, RecordValue::Int(value)) if value >= 0 => {
                arguments[index] = value
            }
            (wings_abi::ValueType::Bool, RecordValue::Bool(value)) => {
                arguments[index] = i64::from(value)
            }
            (wings_abi::ValueType::Unit, RecordValue::Unit) => arguments[index] = 0,
            (
                ty @ (wings_abi::ValueType::ListRef
                | wings_abi::ValueType::BytesRef
                | wings_abi::ValueType::StringRef),
                RecordValue::Ref(value),
            ) => {
                unsafe { pointer.as_ref() }
                    .ok_or(STATUS_INVALID_ROOT)?
                    .typed_reference(ty, value.encoded() as i64)?;
                references[index] = value.encoded();
            }
            _ => return Err(STATUS_INVALID_ROOT),
        }
        *count += 1;
    }
    Ok(())
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
        let start = payload.len();
        let mut current = *argument;
        loop {
            crate::execution::runtime_checkpoint(context, true)?;
            let heap = unsafe { context.heap_data.cast::<AotHeap>().as_ref() }
                .ok_or(STATUS_INVALID_ROOT)?;
            let Some((value, tail)) = heap.list_node(current)? else {
                break;
            };
            payload
                .values
                .try_push(value)
                .map_err(|_| wings_abi::STATUS_SEND_REJECTED)?;
            current = tail;
        }
        *argument = ListPayload::encode(start, payload.len() - start);
    }
    Ok(payload)
}

#[cfg(test)]
pub(crate) unsafe fn export_bytes(
    context: &mut ScalarContext,
    arguments: &mut [i64],
    mask: u64,
) -> Result<BytePayload, u32> {
    unsafe { export_buffers(context, arguments, mask, 0) }
}

pub(crate) unsafe fn export_buffers(
    context: &mut ScalarContext,
    arguments: &mut [i64],
    bytes_mask: u64,
    string_mask: u64,
) -> Result<BytePayload, u32> {
    let mask = bytes_mask | string_mask;
    if arguments.len() > 64
        || bytes_mask & string_mask != 0
        || (arguments.len() < 64 && mask >> arguments.len() != 0)
    {
        return Err(STATUS_INVALID_CONTROL);
    }
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
            let bytes = message_buffer(heap, reference, string_mask & (1u64 << index) != 0)?;
            length = length
                .checked_add(bytes.len())
                .filter(|total| *total <= MAX_MESSAGE_BYTES)
                .ok_or(wings_abi::STATUS_SEND_REJECTED)?;
        }
    }
    let account = unsafe { &*pointer }.account.clone();
    let mut payload = match BytePayload::new(&account, mask, length) {
        Ok(payload) => payload,
        Err(_) => {
            unsafe { &mut *pointer }.heap.drop_spare_buffer();
            BytePayload::new(&account, mask, length).map_err(|_| STATUS_HEAP_EXHAUSTED)?
        }
    };
    payload.string_mask = string_mask;
    for (index, argument) in arguments.iter_mut().enumerate() {
        if mask & (1u64 << index) == 0 {
            continue;
        }
        let reference = unsafe { &*pointer }
            .heap
            .import_reference(*argument as u64)
            .map_err(|_| STATUS_INVALID_ROOT)?;
        let is_string = string_mask & (1u64 << index) != 0;
        let length = message_buffer(unsafe { &*pointer }, reference, is_string)?.len();
        let start = payload.data.len();
        let mut offset = 0;
        while offset < length {
            let batch_chunks = crate::execution::runtime_checkpoint_batch(
                context,
                (length - offset).div_ceil(256),
            )?;
            let end = offset
                .saturating_add(batch_chunks.saturating_mul(256))
                .min(length);
            let bytes = message_buffer(unsafe { &*pointer }, reference, is_string)?;
            payload.data.extend_from_slice(&bytes[offset..end]);
            offset = end;
        }
        *argument = ListPayload::encode(start, length);
    }
    Ok(payload)
}

pub(super) fn message_buffer(
    heap: &AotHeap,
    reference: HeapRef,
    string: bool,
) -> Result<&[u8], u32> {
    if string {
        heap.heap.string_bytes(reference)
    } else {
        heap.heap.bytes(reference)
    }
    .map_err(|_| STATUS_INVALID_ROOT)
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
    let mut offset = 0;
    while offset < data.len() {
        let batch_chunks = crate::execution::runtime_checkpoint_batch(
            context,
            (data.len() - offset).div_ceil(256),
        )?;
        let end = offset
            .saturating_add(batch_chunks.saturating_mul(256))
            .min(data.len());
        buffer.data.extend_from_slice(&data[offset..end]);
        offset = end;
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
    lists: &ListPayload,
    bytes: &BytePayload,
) -> Result<(), u32> {
    if (lists.mask | bytes.mask) == 0 && lists.is_empty() && bytes.is_trivially_empty() {
        return if arguments.len() <= 64 {
            Ok(())
        } else {
            Err(STATUS_INVALID_CONTROL)
        };
    }
    if !lists.valid(arguments, lists.mask)
        || !bytes.valid(arguments, bytes.mask)
        || lists.mask & bytes.mask != 0
    {
        return Err(STATUS_INVALID_CONTROL);
    }
    for (index, argument) in arguments.iter_mut().enumerate() {
        let bit = 1u64 << index;
        if (lists.mask | bytes.mask) & bit == 0 {
            continue;
        }
        if context.heap_data.is_null() {
            return Err(STATUS_HEAP_EXHAUSTED);
        }
        crate::execution::runtime_checkpoint(context, true)?;
        if bytes.mask & bit != 0 {
            let (start, length) = ListPayload::segment(*argument);
            let data = &bytes.as_slice()[start..start + length];
            let encoded = if bytes.string_mask & bit != 0 {
                unsafe {
                    string_literal(context, [data.as_ptr() as usize as i64, length as i64, 0])
                }?
            } else {
                unsafe { import_bytes(context, data) }?
            };
            unsafe { root_incoming(context, index, encoded) }?;
            *argument = encoded as i64;
            continue;
        }
        let layout = unsafe { &*context.heap_data.cast::<AotHeap>() }.empty_list;
        let mut encoded = unsafe { allocate_managed(context, layout, &[RecordValue::Unit]) }?;
        unsafe { root_incoming(context, index, encoded) }?;
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
    let slots = match heap.incoming.as_mut() {
        Some(slots) => slots,
        None => {
            let layout = std::alloc::Layout::new::<[Option<HeapRef>; 64]>();
            let pointer = unsafe { std::alloc::alloc(layout) }.cast::<[Option<HeapRef>; 64]>();
            if pointer.is_null() {
                return Err(STATUS_HEAP_EXHAUSTED);
            }
            unsafe { pointer.write([None; 64]) };
            heap.incoming.insert(unsafe { Box::from_raw(pointer) })
        }
    };
    slots[index] = Some(reference);
    heap.incoming_mask |= 1u64 << index;
    Ok(())
}
