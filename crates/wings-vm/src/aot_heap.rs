use std::alloc::{Layout, alloc};

use wings_abi::{
    HEAP_COLLECT, HEAP_EMPTY_INTS, HEAP_HEAD_INT, HEAP_IS_EMPTY_INTS, HEAP_PREPEND_INT,
    HEAP_READ_INT, HEAP_RECORD_INT, HEAP_TAIL_INTS, MAX_SHADOW_ROOTS, STATUS_EMPTY_LIST,
    STATUS_HEAP_EXHAUSTED, STATUS_INVALID_CONTROL, STATUS_INVALID_ROOT, STATUS_OK, ScalarContext,
    ShadowFrame,
};

use crate::{
    FieldType, HeapRef, HeapStats, RecordHeap, RecordLayout, RecordValue, Reservation,
    ResourceAccount, Resources, ShadowRoots, StackPool,
};

pub(crate) struct AotHeap {
    pub(crate) pool: Option<StackPool>,
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
                pool: None,
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
        if ty == wings_abi::ValueType::ListRef {
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
}

pub(crate) unsafe fn import_arguments(
    context: &mut ScalarContext,
    arguments: &mut [i64],
    mask: u64,
) -> Result<(), u32> {
    for (index, argument) in arguments.iter_mut().enumerate() {
        if mask & (1u64 << index) == 0 {
            continue;
        }
        if context.heap_data.is_null() {
            return Err(STATUS_HEAP_EXHAUSTED);
        }
        crate::execution::runtime_checkpoint(context, true)?;
        let layout = unsafe { &*context.heap_data.cast::<AotHeap>() }.layout;
        let encoded = unsafe { allocate_managed(context, layout, &[RecordValue::Int(*argument)]) }?;
        let heap = unsafe { &mut *context.heap_data.cast::<AotHeap>() };
        let reference = heap
            .heap
            .import_reference(encoded)
            .map_err(|_| STATUS_INVALID_ROOT)?;
        heap.incoming[index] = Some(reference);
        *argument = encoded as i64;
    }
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
        unsafe { import_arguments(&mut context, &mut arguments, u64::MAX) }.unwrap();
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
        unsafe { import_arguments(&mut context, &mut arguments, 3) }.unwrap();
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
