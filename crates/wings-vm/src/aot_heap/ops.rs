use super::*;

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
    let mut buffer = if !unsafe { &*pointer }.heap.has_free_slot() {
        match unsafe { collect_managed_recycling(context, length) }? {
            Some(recycled) => recycled,
            None => unsafe { &*pointer }
                .heap
                .prepare_bytes(length)
                .map_err(|_| STATUS_HEAP_EXHAUSTED)?,
        }
    } else {
        match unsafe { &*pointer }.heap.prepare_bytes(length) {
            Ok(buffer) => buffer,
            Err(_) => match unsafe { collect_managed_recycling(context, length) }? {
                Some(recycled) => recycled,
                None => unsafe { &*pointer }
                    .heap
                    .prepare_bytes(length)
                    .map_err(|_| STATUS_HEAP_EXHAUSTED)?,
            },
        }
    };
    let mut start = 0;
    while start < length {
        let batch_chunks =
            crate::execution::runtime_checkpoint_batch(context, (length - start).div_ceil(256))?;
        let end = start
            .saturating_add(batch_chunks.saturating_mul(256))
            .min(length);
        if let Some(source) = source {
            let heap = unsafe { &*pointer };
            let bytes = heap.heap.bytes(source).map_err(|_| STATUS_INVALID_ROOT)?;
            buffer.data.extend_from_slice(&bytes[start..end]);
        } else {
            buffer.data.resize(end, value as u8);
        }
        start = end;
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

unsafe fn string_count(
    context: &mut ScalarContext,
    reference: HeapRef,
    from_bytes: bool,
) -> Result<u64, u32> {
    let pointer = context.heap_data.cast::<AotHeap>();
    let mut offset = 0;
    let mut count = 0;
    loop {
        crate::execution::runtime_checkpoint(context, true)?;
        let heap = unsafe { &*pointer };
        let data = if from_bytes {
            heap.heap.bytes(reference)
        } else {
            heap.heap.string_bytes(reference)
        }
        .map_err(|_| STATUS_INVALID_ROOT)?;
        if offset == data.len() {
            return Ok(count);
        }
        let end = (offset + 256).min(data.len());
        let chunk = &data[offset..end];
        let valid = match std::str::from_utf8(chunk) {
            Ok(text) => text,
            Err(error) if error.error_len().is_none() && end < data.len() => {
                std::str::from_utf8(&chunk[..error.valid_up_to()])
                    .map_err(|_| wings_abi::STATUS_INVALID_STRING)?
            }
            Err(_) => return Err(wings_abi::STATUS_INVALID_STRING),
        };
        count += valid.chars().count() as u64;
        offset += valid.len();
    }
}

unsafe fn string_byte_range(
    context: &mut ScalarContext,
    reference: HeapRef,
    start: usize,
    end: usize,
) -> Result<(usize, usize), u32> {
    let pointer = context.heap_data.cast::<AotHeap>();
    let mut byte_offset = 0;
    let mut scalar_index = 0;
    let mut start_offset = (start == 0).then_some(0);
    let mut end_offset = (end == 0).then_some(0);
    loop {
        crate::execution::runtime_checkpoint(context, true)?;
        let (chunk_bytes, found_start, found_end, total_bytes) = {
            let heap = unsafe { &*pointer };
            let data = heap
                .heap
                .string_bytes(reference)
                .map_err(|_| STATUS_INVALID_ROOT)?;
            if byte_offset == data.len() {
                (0, None, None, data.len())
            } else {
                let chunk_end = (byte_offset + 256).min(data.len());
                let chunk = &data[byte_offset..chunk_end];
                let text = match std::str::from_utf8(chunk) {
                    Ok(text) => text,
                    Err(error) if error.error_len().is_none() && chunk_end < data.len() => {
                        std::str::from_utf8(&chunk[..error.valid_up_to()])
                            .map_err(|_| wings_abi::STATUS_INVALID_STRING)?
                    }
                    Err(_) => return Err(wings_abi::STATUS_INVALID_STRING),
                };
                let mut found_start = None;
                let mut found_end = None;
                for (offset, character) in text.char_indices() {
                    if scalar_index == start {
                        found_start = Some(byte_offset + offset);
                    }
                    if scalar_index == end {
                        found_end = Some(byte_offset + offset);
                    }
                    scalar_index += 1;
                    let next_offset = byte_offset + offset + character.len_utf8();
                    if scalar_index == start {
                        found_start = Some(next_offset);
                    }
                    if scalar_index == end {
                        found_end = Some(next_offset);
                    }
                }
                (text.len(), found_start, found_end, data.len())
            }
        };
        if let Some(offset) = found_start {
            start_offset = Some(offset);
        }
        if let Some(offset) = found_end {
            end_offset = Some(offset);
        }
        if let (Some(start), Some(end)) = (start_offset, end_offset) {
            return Ok((start, end));
        }
        if byte_offset == total_bytes {
            if scalar_index == start {
                start_offset = Some(total_bytes);
            }
            if scalar_index == end {
                end_offset = Some(total_bytes);
            }
            return match (start_offset, end_offset) {
                (Some(start), Some(end)) => Ok((start, end)),
                _ => Err(wings_abi::STATUS_INVALID_STRING_INDEX),
            };
        }
        byte_offset += chunk_bytes;
    }
}

unsafe fn string_at(
    context: &mut ScalarContext,
    reference: HeapRef,
    index: usize,
) -> Result<u64, u32> {
    let end = index
        .checked_add(1)
        .ok_or(wings_abi::STATUS_INVALID_STRING_INDEX)?;
    let (start, end) = unsafe { string_byte_range(context, reference, index, end) }?;
    let heap = unsafe { &*context.heap_data.cast::<AotHeap>() };
    let data = heap
        .heap
        .string_bytes(reference)
        .map_err(|_| STATUS_INVALID_ROOT)?;
    let text =
        std::str::from_utf8(&data[start..end]).map_err(|_| wings_abi::STATUS_INVALID_STRING)?;
    Ok(text.chars().next().ok_or(wings_abi::STATUS_INVALID_ROOT)? as u32 as u64)
}

unsafe fn string_call(
    context: &mut ScalarContext,
    operation: u32,
    arguments: [i64; 3],
) -> Result<u64, u32> {
    use wings_abi::{
        HEAP_CONCAT_STRINGS, HEAP_STRING_AT, HEAP_STRING_FROM_BYTES, HEAP_STRING_LEN,
        HEAP_STRING_SLICE, STATUS_INVALID_STRING, STATUS_INVALID_STRING_INDEX,
    };
    let pointer = context.heap_data.cast::<AotHeap>();
    let from_bytes = operation == HEAP_STRING_FROM_BYTES;
    let source = unsafe { &*pointer }
        .heap
        .import_reference(arguments[0] as u64)
        .map_err(|_| STATUS_INVALID_ROOT)?;
    if operation == HEAP_STRING_AT {
        let index = usize::try_from(arguments[1]).map_err(|_| STATUS_INVALID_STRING_INDEX)?;
        return unsafe { string_at(context, source, index) };
    }
    let mut sources = [Some(source), None];
    let mut ranges = [(0, 0); 2];
    if operation == HEAP_STRING_LEN || from_bytes {
        let count = unsafe { string_count(context, source, from_bytes) }?;
        if operation == HEAP_STRING_LEN {
            return Ok(count);
        }
    }
    if operation == HEAP_CONCAT_STRINGS {
        sources[1] = Some(
            unsafe { &*pointer }
                .heap
                .import_reference(arguments[1] as u64)
                .map_err(|_| STATUS_INVALID_ROOT)?,
        );
    }
    let mut lengths = [0usize; 2];
    for (index, reference) in sources.iter().enumerate() {
        if let Some(reference) = reference {
            let heap = unsafe { &*pointer };
            lengths[index] = if from_bytes {
                heap.heap.bytes(*reference)
            } else {
                heap.heap.string_bytes(*reference)
            }
            .map_err(|_| STATUS_INVALID_ROOT)?
            .len();
            ranges[index] = (0, lengths[index]);
        }
    }
    if operation == HEAP_STRING_SLICE {
        let start = usize::try_from(arguments[1]).map_err(|_| STATUS_INVALID_STRING_INDEX)?;
        let length = usize::try_from(arguments[2]).map_err(|_| STATUS_INVALID_STRING_INDEX)?;
        let end = start
            .checked_add(length)
            .ok_or(STATUS_INVALID_STRING_INDEX)?;
        let (byte_start, byte_end) = unsafe { string_byte_range(context, source, start, end) }?;
        ranges[0] = (byte_start, byte_end - byte_start);
        lengths[0] = ranges[0].1;
    }
    let length = lengths[0]
        .checked_add(lengths[1])
        .filter(|length| *length <= crate::heap::MAX_BYTES)
        .ok_or(STATUS_INVALID_STRING)?;
    let mut buffer = if !unsafe { &*pointer }.heap.has_free_slot() {
        match unsafe { collect_managed_recycling(context, length) }? {
            Some(recycled) => recycled,
            None => unsafe { &*pointer }
                .heap
                .prepare_bytes(length)
                .map_err(|_| STATUS_HEAP_EXHAUSTED)?,
        }
    } else {
        match unsafe { &*pointer }.heap.prepare_bytes(length) {
            Ok(buffer) => buffer,
            Err(_) => match unsafe { collect_managed_recycling(context, length) }? {
                Some(recycled) => recycled,
                None => unsafe { &*pointer }
                    .heap
                    .prepare_bytes(length)
                    .map_err(|_| STATUS_HEAP_EXHAUSTED)?,
            },
        }
    };
    for (reference, (source_offset, length)) in sources.into_iter().zip(ranges) {
        if let Some(reference) = reference {
            for chunk_offset in (0..length).step_by(256) {
                crate::execution::runtime_checkpoint(context, true)?;
                let heap = unsafe { &*pointer };
                let data = if from_bytes {
                    heap.heap.bytes(reference)
                } else {
                    heap.heap.string_bytes(reference)
                }
                .map_err(|_| STATUS_INVALID_ROOT)?;
                buffer.data.extend_from_slice(
                    &data[source_offset + chunk_offset
                        ..source_offset + (chunk_offset + 256).min(length)],
                );
            }
        }
    }
    let mut buffer = Some(buffer);
    for attempt in 0..2 {
        let mut cursor = 0;
        loop {
            crate::execution::runtime_checkpoint(context, true)?;
            let heap = unsafe { &mut *pointer };
            let result = if operation == wings_abi::HEAP_STRING_BYTES {
                heap.heap.allocate_bytes_chunk(&mut buffer, &mut cursor)
            } else {
                heap.heap.allocate_string_chunk(&mut buffer, &mut cursor)
            };
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

pub(super) unsafe fn string_literal(
    context: &mut ScalarContext,
    arguments: [i64; 3],
) -> Result<u64, u32> {
    let length = usize::try_from(arguments[1])
        .ok()
        .filter(|length| *length <= crate::heap::MAX_BYTES)
        .ok_or(wings_abi::STATUS_INVALID_STRING)?;
    if arguments[0] == 0 {
        return Err(wings_abi::STATUS_INVALID_STRING);
    }
    let pointer = context.heap_data.cast::<AotHeap>();
    let mut buffer = if !unsafe { &*pointer }.heap.has_free_slot() {
        match unsafe { collect_managed_recycling(context, length) }? {
            Some(recycled) => recycled,
            None => unsafe { &*pointer }
                .heap
                .prepare_bytes(length)
                .map_err(|_| STATUS_HEAP_EXHAUSTED)?,
        }
    } else {
        match unsafe { &*pointer }.heap.prepare_bytes(length) {
            Ok(buffer) => buffer,
            Err(_) => match unsafe { collect_managed_recycling(context, length) }? {
                Some(recycled) => recycled,
                None => unsafe { &*pointer }
                    .heap
                    .prepare_bytes(length)
                    .map_err(|_| STATUS_HEAP_EXHAUSTED)?,
            },
        }
    };
    let mut offset = 0;
    while offset < length {
        crate::execution::runtime_checkpoint(context, true)?;
        let data =
            unsafe { std::slice::from_raw_parts(arguments[0] as usize as *const u8, length) };
        let end = (offset + 256).min(length);
        let chunk = &data[offset..end];
        let valid = match std::str::from_utf8(chunk) {
            Ok(text) => text,
            Err(error) if error.error_len().is_none() && end < length => {
                std::str::from_utf8(&chunk[..error.valid_up_to()])
                    .map_err(|_| wings_abi::STATUS_INVALID_STRING)?
            }
            Err(_) => return Err(wings_abi::STATUS_INVALID_STRING),
        };
        buffer.data.extend_from_slice(valid.as_bytes());
        offset += valid.len();
    }
    let mut buffer = Some(buffer);
    for attempt in 0..2 {
        let mut cursor = 0;
        loop {
            crate::execution::runtime_checkpoint(context, true)?;
            match unsafe { &mut *pointer }
                .heap
                .allocate_string_chunk(&mut buffer, &mut cursor)
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

fn format_i64_decimal(value: i64, out: &mut [u8; 24]) -> &[u8] {
    let negative = value < 0;
    let mut magnitude = value.unsigned_abs();
    let mut cursor = out.len();
    loop {
        cursor -= 1;
        out[cursor] = b'0' + (magnitude % 10) as u8;
        magnitude /= 10;
        if magnitude == 0 {
            break;
        }
    }
    if negative {
        cursor -= 1;
        out[cursor] = b'-';
    }
    &out[cursor..]
}

unsafe fn allocate_short_string(context: &mut ScalarContext, text: &[u8]) -> Result<u64, u32> {
    let length = text.len();
    let pointer = context.heap_data.cast::<AotHeap>();
    let mut buffer = if !unsafe { &*pointer }.heap.has_free_slot() {
        match unsafe { collect_managed_recycling(context, length) }? {
            Some(recycled) => recycled,
            None => unsafe { &*pointer }
                .heap
                .prepare_bytes(length)
                .map_err(|_| STATUS_HEAP_EXHAUSTED)?,
        }
    } else {
        match unsafe { &*pointer }.heap.prepare_bytes(length) {
            Ok(buffer) => buffer,
            Err(_) => match unsafe { collect_managed_recycling(context, length) }? {
                Some(recycled) => recycled,
                None => unsafe { &*pointer }
                    .heap
                    .prepare_bytes(length)
                    .map_err(|_| STATUS_HEAP_EXHAUSTED)?,
            },
        }
    };
    crate::execution::runtime_checkpoint(context, true)?;
    buffer.data.extend_from_slice(text);
    let mut buffer = Some(buffer);
    for attempt in 0..2 {
        let mut cursor = 0;
        loop {
            crate::execution::runtime_checkpoint(context, true)?;
            match unsafe { &mut *pointer }
                .heap
                .allocate_string_chunk(&mut buffer, &mut cursor)
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

unsafe fn bytes_literal(context: &mut ScalarContext, arguments: [i64; 3]) -> Result<u64, u32> {
    let length = usize::try_from(arguments[1])
        .ok()
        .filter(|length| *length <= crate::heap::MAX_BYTES)
        .ok_or(wings_abi::STATUS_INVALID_BYTES)?;
    if arguments[0] == 0 {
        return Err(wings_abi::STATUS_INVALID_BYTES);
    }
    let pointer = context.heap_data.cast::<AotHeap>();
    let mut buffer = if !unsafe { &*pointer }.heap.has_free_slot() {
        match unsafe { collect_managed_recycling(context, length) }? {
            Some(recycled) => recycled,
            None => unsafe { &*pointer }
                .heap
                .prepare_bytes(length)
                .map_err(|_| STATUS_HEAP_EXHAUSTED)?,
        }
    } else {
        match unsafe { &*pointer }.heap.prepare_bytes(length) {
            Ok(buffer) => buffer,
            Err(_) => match unsafe { collect_managed_recycling(context, length) }? {
                Some(recycled) => recycled,
                None => unsafe { &*pointer }
                    .heap
                    .prepare_bytes(length)
                    .map_err(|_| STATUS_HEAP_EXHAUSTED)?,
            },
        }
    };
    for offset in (0..length).step_by(256) {
        crate::execution::runtime_checkpoint(context, true)?;
        let data =
            unsafe { std::slice::from_raw_parts(arguments[0] as usize as *const u8, length) };
        let end = (offset + 256).min(length);
        buffer.data.extend_from_slice(&data[offset..end]);
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

unsafe fn equal_strings(context: &mut ScalarContext, arguments: [i64; 3]) -> Result<u64, u32> {
    let pointer = context.heap_data.cast::<AotHeap>();
    let references = {
        let heap = unsafe { &*pointer };
        [arguments[0], arguments[1]].map(|encoded| {
            heap.heap
                .import_reference(encoded as u64)
                .map_err(|_| STATUS_INVALID_ROOT)
        })
    };
    let [left, right] = references;
    let left = left?;
    let right = right?;
    crate::execution::runtime_checkpoint(context, true)?;
    let lengths = {
        let heap = unsafe { &*pointer };
        [left, right].map(|reference| {
            heap.heap
                .string_bytes(reference)
                .map(<[u8]>::len)
                .map_err(|_| STATUS_INVALID_ROOT)
        })
    };
    let [left_length, right_length] = lengths;
    let length = left_length?;
    if length != right_length? {
        return Ok(0);
    }
    for start in (0..length).step_by(256) {
        crate::execution::runtime_checkpoint(context, true)?;
        let heap = unsafe { &*pointer };
        let first = heap
            .heap
            .string_bytes(left)
            .map_err(|_| STATUS_INVALID_ROOT)?;
        let second = heap
            .heap
            .string_bytes(right)
            .map_err(|_| STATUS_INVALID_ROOT)?;
        let end = (start + 256).min(length);
        if first[start..end] != second[start..end] {
            return Ok(0);
        }
    }
    Ok(1)
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
    if operation == wings_abi::HEAP_EQUAL_STRINGS {
        let arguments = unsafe { *(argument as usize as *const [i64; 3]) };
        return match unsafe { equal_strings(context, arguments) } {
            Ok(value) => {
                unsafe { *output = value };
                STATUS_OK
            }
            Err(status) => status,
        };
    }
    if matches!(
        operation,
        wings_abi::HEAP_RECORD_NEW | wings_abi::HEAP_RECORD_FIELD
    ) {
        let arguments = argument as usize as *const u64;
        let schema = unsafe { *arguments };
        let Some(named) = usize::try_from(schema)
            .ok()
            .and_then(|index| unsafe { &*pointer }.named.get(index))
            .copied()
        else {
            return STATUS_INVALID_ROOT;
        };
        let result = if operation == wings_abi::HEAP_RECORD_NEW {
            let mut values = [RecordValue::Unit; 8];
            for (index, value) in values[..named.count].iter_mut().enumerate() {
                let encoded = unsafe { *arguments.add(index + 1) } as i64;
                let ty = named.types[index];
                *value = if ty.is_reference() {
                    match unsafe { &*pointer }.typed_reference(ty, encoded) {
                        Ok(reference) => RecordValue::Ref(reference),
                        Err(status) => return status,
                    }
                } else if ty == wings_abi::ValueType::Pid && encoded >= 0 {
                    RecordValue::Int(encoded)
                } else if ty.accepts(encoded) {
                    match ty {
                        wings_abi::ValueType::Int => RecordValue::Int(encoded),
                        wings_abi::ValueType::Bool => RecordValue::Bool(encoded != 0),
                        _ => RecordValue::Unit,
                    }
                } else {
                    return STATUS_INVALID_ROOT;
                };
            }
            unsafe { allocate_managed(context, named.layout, &values[..named.count]) }
        } else {
            let reference = unsafe { *arguments.add(1) };
            let field = unsafe { *arguments.add(2) };
            let Ok(index) = usize::try_from(field) else {
                return STATUS_INVALID_ROOT;
            };
            if index >= named.count {
                return STATUS_INVALID_ROOT;
            }
            let heap = unsafe { &*pointer };
            let Ok(reference) = heap.typed_reference(
                wings_abi::ValueType::RecordRef(schema as u16),
                reference as i64,
            ) else {
                return STATUS_INVALID_ROOT;
            };
            heap.heap
                .get(reference, index)
                .map(|value| match value {
                    RecordValue::Int(value) => value as u64,
                    RecordValue::Bool(value) => u64::from(value),
                    RecordValue::Unit => 0,
                    RecordValue::Ref(value) => value.encoded(),
                })
                .map_err(|_| STATUS_INVALID_ROOT)
        };
        return match result {
            Ok(value) => {
                unsafe { *output = value };
                STATUS_OK
            }
            Err(status) => status,
        };
    }
    if (wings_abi::HEAP_STRING_FROM_BYTES..=wings_abi::HEAP_STRING_LITERAL).contains(&operation)
        || matches!(
            operation,
            wings_abi::HEAP_STRING_AT | wings_abi::HEAP_STRING_SLICE
        )
    {
        let arguments = unsafe { *(argument as usize as *const [i64; 3]) };
        let result = if operation == wings_abi::HEAP_STRING_LITERAL {
            unsafe { string_literal(context, arguments) }
        } else {
            unsafe { string_call(context, operation, arguments) }
        };
        return match result {
            Ok(value) => {
                unsafe { *output = value };
                STATUS_OK
            }
            Err(status) => status,
        };
    }
    if operation == wings_abi::HEAP_BYTES_LITERAL {
        let arguments = unsafe { *(argument as usize as *const [i64; 3]) };
        return match unsafe { bytes_literal(context, arguments) } {
            Ok(value) => {
                unsafe { *output = value };
                STATUS_OK
            }
            Err(status) => status,
        };
    }
    if matches!(
        operation,
        wings_abi::HEAP_INT_TO_STRING | wings_abi::HEAP_BOOL_TO_STRING
    ) {
        let mut decimal = [0u8; 24];
        let text: &[u8] = if operation == wings_abi::HEAP_INT_TO_STRING {
            format_i64_decimal(argument as i64, &mut decimal)
        } else {
            match argument {
                0 => b"false",
                1 => b"true",
                _ => return STATUS_INVALID_ROOT,
            }
        };
        return match unsafe { allocate_short_string(context, text) } {
            Ok(value) => {
                unsafe { *output = value };
                STATUS_OK
            }
            Err(status) => status,
        };
    }
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
    if matches!(operation, HEAP_EMPTY_INTS | HEAP_PREPEND_INT) {
        let (layout, values, count) = {
            let heap = unsafe { &*pointer };
            match operation {
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
        _ => return STATUS_INVALID_CONTROL,
    };
    unsafe { *output = value };
    STATUS_OK
}
