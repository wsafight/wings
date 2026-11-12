use std::alloc::{Layout, alloc};

use wings_abi::{
    HEAP_COLLECT, HEAP_READ_INT, HEAP_RECORD_INT, MAX_SHADOW_ROOTS, STATUS_HEAP_EXHAUSTED,
    STATUS_INVALID_CONTROL, STATUS_INVALID_ROOT, STATUS_OK, ScalarContext, ShadowFrame,
};

use crate::{
    FieldType, HeapStats, RecordHeap, RecordLayout, RecordValue, Reservation, ResourceAccount,
    Resources, ShadowRoots,
};

pub(crate) struct AotHeap {
    heap: RecordHeap,
    roots: ShadowRoots,
    layout: RecordLayout,
    _metadata: Reservation,
}

impl AotHeap {
    pub(crate) fn new(account: &ResourceAccount, capacity: usize) -> Result<Box<Self>, String> {
        let mut heap = RecordHeap::new(account, capacity, 1)?;
        let metadata = account.reserve(Resources {
            bytes: (std::mem::size_of::<Self>()
                - std::mem::size_of::<RecordHeap>()
                - std::mem::size_of::<ShadowRoots>()) as u64,
            ..Resources::default()
        })?;
        let layout = heap.register_layout(&[FieldType::Int])?;
        let roots = ShadowRoots::new(&heap, MAX_SHADOW_ROOTS)?;
        let pointer = unsafe { alloc(Layout::new::<Self>()) }.cast::<Self>();
        if pointer.is_null() {
            return Err("AOT heap control allocation failed".into());
        }
        unsafe {
            pointer.write(Self {
                heap,
                roots,
                layout,
                _metadata: metadata,
            })
        };
        Ok(unsafe { Box::from_raw(pointer) })
    }

    pub(crate) fn stats(&self) -> HeapStats {
        self.heap.stats()
    }

    unsafe fn collect(&mut self, mut frame: *const ShadowFrame) -> Result<(), String> {
        let mut references = [None; MAX_SHADOW_ROOTS];
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
        self.roots.replace(&references[..count]);
        self.heap.collect(&self.roots)?;
        Ok(())
    }
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
    let heap = unsafe { &mut *context.heap_data.cast::<AotHeap>() };
    let value = match operation {
        HEAP_RECORD_INT => {
            let values = [RecordValue::Int(argument as i64)];
            let reference = match heap.heap.allocate(heap.layout, &values) {
                Ok(reference) => reference,
                Err(_) => {
                    if unsafe { heap.collect(context.roots) }.is_err() {
                        return STATUS_INVALID_ROOT;
                    }
                    match heap.heap.allocate(heap.layout, &values) {
                        Ok(reference) => reference,
                        Err(_) => return STATUS_HEAP_EXHAUSTED,
                    }
                }
            };
            reference.encoded()
        }
        HEAP_READ_INT => {
            let Ok(reference) = heap.heap.import_reference(argument) else {
                return STATUS_INVALID_ROOT;
            };
            let Ok(RecordValue::Int(value)) = heap.heap.get(reference, 0) else {
                return STATUS_INVALID_ROOT;
            };
            value as u64
        }
        HEAP_COLLECT => {
            if unsafe { heap.collect(context.roots) }.is_err() {
                return STATUS_INVALID_ROOT;
            }
            0
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
