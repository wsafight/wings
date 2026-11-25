use std::alloc::{Layout, alloc};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{Reservation, ResourceAccount, ResourceLevel, Resources};

const MAX_FIELDS: usize = 8;
const INLINE_RECORD_FIELDS: usize = 2;
const MAX_RECORDS: usize = 1024;
const MAX_LAYOUTS: usize = 128;
const MAX_ROOTS: usize = 64;
const WORK_CHUNK: usize = 16;
const BYTES_LAYOUT: u16 = u16::MAX;
const STRING_LAYOUT: u16 = u16::MAX - 1;
pub(crate) const MAX_BYTES: usize = 65536;
static NEXT_HEAP: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldType {
    Int,
    Bool,
    Unit,
    Ref,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeapRef {
    heap: u64,
    slot: u32,
    generation: u32,
}

impl HeapRef {
    pub(crate) fn encoded(self) -> u64 {
        (u64::from(self.generation) << 32) | u64::from(self.slot)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordLayout {
    heap: u64,
    index: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordValue {
    Int(i64),
    Bool(bool),
    Unit,
    Ref(HeapRef),
}

#[derive(Clone, Copy)]
struct Schema {
    fields: [FieldType; MAX_FIELDS],
    count: usize,
}

struct OverflowFields {
    data: [u64; MAX_FIELDS - INLINE_RECORD_FIELDS],
    _charge: Reservation,
}

const OVERFLOW_FIELD_BYTES: u64 = std::mem::size_of::<OverflowFields>() as u64;

struct Slot {
    inline_fields: [u64; INLINE_RECORD_FIELDS],
    overflow_fields: Option<Box<OverflowFields>>,
    generation: u32,
    layout: u16,
    occupied: bool,
    marked: bool,
}

impl Slot {
    fn empty() -> Self {
        Self {
            inline_fields: [0; INLINE_RECORD_FIELDS],
            overflow_fields: None,
            generation: 1,
            layout: 0,
            occupied: false,
            marked: false,
        }
    }

    fn field(&self, index: usize) -> u64 {
        if index < INLINE_RECORD_FIELDS {
            self.inline_fields[index]
        } else {
            self.overflow_fields
                .as_ref()
                .expect("wide record fields")
                .data[index - INLINE_RECORD_FIELDS]
        }
    }

    fn set_fields(&mut self, fields: &[u64; MAX_FIELDS]) {
        self.inline_fields
            .copy_from_slice(&fields[..INLINE_RECORD_FIELDS]);
        if let Some(overflow) = self.overflow_fields.as_mut() {
            overflow
                .data
                .copy_from_slice(&fields[INLINE_RECORD_FIELDS..]);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeapStats {
    pub capacity: usize,
    pub allocated: usize,
    pub retired: usize,
    pub layouts: usize,
    pub collections: u64,
    pub charged_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CollectionStats {
    pub before: usize,
    pub retained: usize,
    pub reclaimed: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByteStateSnapshot {
    pub length: usize,
    pub prefix: [u8; 32],
}

pub struct RecordHeap {
    identity: u64,
    slots: Box<[Slot]>,
    schemas: Box<[Schema]>,
    worklist: Box<[usize]>,
    buffers: Box<[Option<ByteBuffer>]>,
    spare_buffer: Option<ByteBuffer>,
    buffer_bytes: u64,
    layout_count: usize,
    collections: u64,
    allocated: usize,
    retired: usize,
    free_hint: usize,
    account: ResourceAccount,
    charge: Reservation,
    overflow_bytes: u64,
}

pub(crate) struct ByteBuffer {
    pub(crate) data: Vec<u8>,
    heap: u64,
    _charge: Reservation,
}

pub struct ShadowRoots {
    heap: u64,
    slots: Box<[Option<HeapRef>]>,
    max_capacity: usize,
    count: usize,
    charge: Reservation,
}

pub struct RootFrame<'roots> {
    roots: &'roots mut ShadowRoots,
    base: usize,
}

pub(crate) struct Collection {
    phase: CollectionPhase,
    cursor: usize,
    pending: usize,
    before: usize,
    retained: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CollectionPhase {
    Clear,
    Roots,
    Trace,
    Sweep,
}

impl RecordHeap {
    pub(crate) fn import_reference(&self, encoded: u64) -> Result<HeapRef, String> {
        let reference = self.decode_reference(encoded);
        self.validate(reference)?;
        Ok(reference)
    }
    pub fn new(
        account: &ResourceAccount,
        capacity: usize,
        layout_capacity: usize,
    ) -> Result<Self, String> {
        if account.level() != ResourceLevel::Process {
            return Err("record heap requires a process resource account".into());
        }
        if !(1..=MAX_RECORDS).contains(&capacity) || !(1..=MAX_LAYOUTS).contains(&layout_capacity) {
            return Err("record heap requires 1..=1024 records and 1..=128 layouts".into());
        }
        let bytes = std::mem::size_of::<Self>()
            + capacity
                * (std::mem::size_of::<Slot>()
                    + std::mem::size_of::<usize>()
                    + std::mem::size_of::<Option<ByteBuffer>>())
            + layout_capacity * std::mem::size_of::<Schema>();
        let charge = account.reserve(Resources {
            bytes: bytes as u64,
            ..Resources::default()
        })?;
        let identity = next_identity()?;
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(capacity)
            .map_err(|error| error.to_string())?;
        if slots.capacity() != capacity {
            return Err("allocator changed record slot capacity".into());
        }
        slots.resize_with(capacity, Slot::empty);
        let schemas = allocate_array(
            layout_capacity,
            Schema {
                fields: [FieldType::Unit; MAX_FIELDS],
                count: 0,
            },
        )?;
        let worklist = allocate_array(capacity, 0)?;
        let mut buffers = Vec::new();
        buffers
            .try_reserve_exact(capacity)
            .map_err(|error| error.to_string())?;
        if buffers.capacity() != capacity {
            return Err("allocator changed fixed buffer table capacity".into());
        }
        buffers.resize_with(capacity, || None);
        Ok(Self {
            identity,
            slots: slots.into_boxed_slice(),
            schemas,
            worklist,
            buffers: buffers.into_boxed_slice(),
            spare_buffer: None,
            buffer_bytes: 0,
            layout_count: 0,
            collections: 0,
            allocated: 0,
            retired: 0,
            free_hint: 0,
            account: account.clone(),
            charge,
            overflow_bytes: 0,
        })
    }

    pub fn register_layout(&mut self, fields: &[FieldType]) -> Result<RecordLayout, String> {
        if fields.len() > MAX_FIELDS {
            return Err("record layout exceeds eight fields".into());
        }
        if let Some(index) = self.schemas[..self.layout_count]
            .iter()
            .position(|schema| schema.fields[..schema.count] == *fields)
        {
            return Ok(RecordLayout {
                heap: self.identity,
                index: index as u16,
            });
        }
        self.register_distinct_layout(fields)
    }

    pub(crate) fn register_distinct_layout(
        &mut self,
        fields: &[FieldType],
    ) -> Result<RecordLayout, String> {
        if fields.len() > MAX_FIELDS {
            return Err("record layout exceeds eight fields".into());
        }
        if self.layout_count == self.schemas.len() {
            return Err("record layout capacity exhausted".into());
        }
        let index = self.layout_count;
        self.schemas[index].fields[..fields.len()].copy_from_slice(fields);
        self.schemas[index].count = fields.len();
        self.layout_count += 1;
        Ok(RecordLayout {
            heap: self.identity,
            index: index as u16,
        })
    }

    pub fn allocate(
        &mut self,
        layout: RecordLayout,
        values: &[RecordValue],
    ) -> Result<HeapRef, String> {
        let mut cursor = 0;
        loop {
            if let Some(reference) = self.allocate_chunk(layout, values, &mut cursor)? {
                return Ok(reference);
            }
        }
    }

    pub(crate) fn allocate_chunk(
        &mut self,
        layout: RecordLayout,
        values: &[RecordValue],
        cursor: &mut usize,
    ) -> Result<Option<HeapRef>, String> {
        let schema = *self.schema(layout)?;
        if values.len() != schema.count {
            return Err("record field count mismatch".into());
        }
        let mut fields = [0; MAX_FIELDS];
        for (index, (ty, value)) in schema.fields[..schema.count].iter().zip(values).enumerate() {
            fields[index] = match (ty, value) {
                (FieldType::Int, RecordValue::Int(value)) => *value as u64,
                (FieldType::Bool, RecordValue::Bool(value)) => u64::from(*value),
                (FieldType::Unit, RecordValue::Unit) => 0,
                (FieldType::Ref, RecordValue::Ref(reference)) => {
                    self.validate(*reference)?;
                    (u64::from(reference.generation) << 32) | u64::from(reference.slot)
                }
                _ => return Err("record field type mismatch".into()),
            };
        }
        let end = (*cursor + WORK_CHUNK).min(self.slots.len());
        let found = if *cursor == 0
            && self.free_hint < end
            && !self.slots[self.free_hint].occupied
            && self.slots[self.free_hint].generation != 0
        {
            Some(self.free_hint)
        } else {
            self.slots[*cursor..end]
                .iter()
                .position(|slot| !slot.occupied && slot.generation != 0)
                .map(|offset| *cursor + offset)
        };
        let Some(index) = found else {
            *cursor = end;
            return if end == self.slots.len() {
                Err("record heap capacity exhausted".into())
            } else {
                Ok(None)
            };
        };
        if schema.count > INLINE_RECORD_FIELDS && self.slots[index].overflow_fields.is_none() {
            let overflow_req = Resources {
                bytes: OVERFLOW_FIELD_BYTES,
                ..Resources::default()
            };
            let charge = match self.account.reserve(overflow_req) {
                Ok(charge) => charge,
                Err(_) if self.spare_buffer.is_some() => {
                    self.spare_buffer = None;
                    self.account.reserve(overflow_req)?
                }
                Err(error) => return Err(error),
            };
            self.slots[index].overflow_fields = Some(Box::new(OverflowFields {
                data: [0; MAX_FIELDS - INLINE_RECORD_FIELDS],
                _charge: charge,
            }));
            self.overflow_bytes += OVERFLOW_FIELD_BYTES;
        }
        let slot = &mut self.slots[index];
        slot.set_fields(&fields);
        slot.layout = layout.index;
        slot.occupied = true;
        self.allocated += 1;
        if self.free_hint == index {
            self.free_hint = index + 1;
        }
        Ok(Some(HeapRef {
            heap: self.identity,
            slot: index as u32,
            generation: slot.generation,
        }))
    }

    pub fn get(&self, reference: HeapRef, field: usize) -> Result<RecordValue, String> {
        let slot = self.validate(reference)?;
        if matches!(slot.layout, BYTES_LAYOUT | STRING_LAYOUT) {
            return Err("byte buffer is not a record".into());
        }
        let schema = &self.schemas[usize::from(slot.layout)];
        if field >= schema.count {
            return Err("record field index out of bounds".into());
        }
        let value = slot.field(field);
        Ok(match schema.fields[field] {
            FieldType::Int => RecordValue::Int(value as i64),
            FieldType::Bool => RecordValue::Bool(value != 0),
            FieldType::Unit => RecordValue::Unit,
            FieldType::Ref => RecordValue::Ref(self.decode_reference(value)),
        })
    }

    pub(crate) fn has_layout(
        &self,
        reference: HeapRef,
        layout: RecordLayout,
    ) -> Result<bool, String> {
        self.schema(layout)?;
        Ok(self.validate(reference)?.layout == layout.index)
    }

    pub fn stats(&self) -> HeapStats {
        let spare_bytes = self
            .spare_buffer
            .as_ref()
            .map_or(0, |buffer| buffer._charge.amount().bytes);
        HeapStats {
            capacity: self.slots.len(),
            allocated: self.allocated,
            retired: self.retired,
            layouts: self.layout_count,
            collections: self.collections,
            charged_bytes: self.charge.amount().bytes
                + self.overflow_bytes
                + self.buffer_bytes
                + spare_bytes,
        }
    }

    pub(crate) fn has_free_slot(&self) -> bool {
        self.allocated + self.retired < self.slots.len()
    }

    pub fn collect(&mut self, roots: &ShadowRoots) -> Result<CollectionStats, String> {
        let mut collection = self.begin_collection(roots)?;
        loop {
            if let Some(stats) = self.collect_chunk(roots, &mut collection) {
                return Ok(stats);
            }
        }
    }

    pub(crate) fn begin_collection(&self, roots: &ShadowRoots) -> Result<Collection, String> {
        if roots.heap != self.identity {
            return Err("shadow roots belong to a different heap".into());
        }
        for reference in roots.slots[..roots.count].iter().flatten() {
            self.validate(*reference)?;
        }
        Ok(Collection {
            phase: CollectionPhase::Clear,
            cursor: 0,
            pending: 0,
            before: self.allocated,
            retained: 0,
        })
    }

    pub(crate) fn collect_chunk(
        &mut self,
        roots: &ShadowRoots,
        collection: &mut Collection,
    ) -> Option<CollectionStats> {
        self.collect_chunk_recycling(roots, collection, 0, &mut None)
    }

    pub(crate) fn collect_chunk_recycling(
        &mut self,
        roots: &ShadowRoots,
        collection: &mut Collection,
        recycle_len: usize,
        recycled: &mut Option<ByteBuffer>,
    ) -> Option<CollectionStats> {
        match collection.phase {
            CollectionPhase::Clear => {
                if collection.cursor == 0 {
                    if let Some(mut spare) = self.spare_buffer.take() {
                        if recycled.is_none()
                            && recycle_len != 0
                            && spare.data.capacity() == recycle_len
                            && spare._charge.amount().bytes == recycle_len as u64
                        {
                            spare.data.clear();
                            spare.heap = self.identity;
                            *recycled = Some(spare);
                        }
                    }
                }
                let end = (collection.cursor + WORK_CHUNK).min(self.slots.len());
                for slot in &mut self.slots[collection.cursor..end] {
                    slot.marked = false;
                }
                collection.cursor = end;
                if end == self.slots.len() {
                    collection.phase = CollectionPhase::Roots;
                    collection.cursor = 0;
                }
            }
            CollectionPhase::Roots => {
                let end = (collection.cursor + WORK_CHUNK).min(roots.count);
                for reference in roots.slots[collection.cursor..end].iter().flatten() {
                    self.mark(reference.slot as usize, &mut collection.pending);
                }
                collection.cursor = end;
                if end == roots.count {
                    collection.phase = CollectionPhase::Trace;
                    collection.cursor = 0;
                }
            }
            CollectionPhase::Trace => {
                for _ in 0..WORK_CHUNK {
                    if collection.pending == 0 {
                        break;
                    }
                    collection.pending -= 1;
                    let slot = &self.slots[self.worklist[collection.pending]];
                    if matches!(slot.layout, BYTES_LAYOUT | STRING_LAYOUT) {
                        continue;
                    }
                    let schema = self.schemas[usize::from(slot.layout)];
                    let mut fields = [0; MAX_FIELDS];
                    for (index, field) in fields.iter_mut().enumerate().take(schema.count) {
                        *field = slot.field(index);
                    }
                    for (ty, value) in schema.fields[..schema.count].iter().zip(fields) {
                        if *ty == FieldType::Ref {
                            let reference = self.decode_reference(value);
                            self.mark(reference.slot as usize, &mut collection.pending);
                        }
                    }
                }
                if collection.pending == 0 {
                    collection.phase = CollectionPhase::Sweep;
                }
            }
            CollectionPhase::Sweep => {
                let end = (collection.cursor + WORK_CHUNK).min(self.slots.len());
                let mut reclaimed_charge: Option<Reservation> = None;
                for (index, slot) in self.slots[collection.cursor..end].iter_mut().enumerate() {
                    if slot.occupied && slot.marked {
                        collection.retained += 1;
                        slot.marked = false;
                    } else if slot.occupied {
                        if let Some(mut buffer) = self.buffers[collection.cursor + index].take() {
                            if recycled.is_none()
                                && recycle_len != 0
                                && buffer.data.capacity() == recycle_len
                                && buffer._charge.amount().bytes == recycle_len as u64
                            {
                                self.buffer_bytes -= buffer._charge.amount().bytes;
                                buffer.data.clear();
                                buffer.heap = self.identity;
                                *recycled = Some(buffer);
                            } else {
                                let ByteBuffer {
                                    data,
                                    heap: _,
                                    _charge: charge,
                                } = buffer;
                                self.buffer_bytes -= charge.amount().bytes;
                                drop(data);
                                match &mut reclaimed_charge {
                                    Some(combined) => combined.merge(charge),
                                    None => reclaimed_charge = Some(charge),
                                }
                            }
                        }
                        if let Some(overflow) = slot.overflow_fields.take() {
                            self.overflow_bytes -= OVERFLOW_FIELD_BYTES;
                            let OverflowFields { data: _, _charge } = *overflow;
                            match &mut reclaimed_charge {
                                Some(combined) => combined.merge(_charge),
                                None => reclaimed_charge = Some(_charge),
                            }
                        }
                        slot.occupied = false;
                        slot.generation = slot.generation.checked_add(1).unwrap_or(0);
                        self.allocated -= 1;
                        if slot.generation == 0 {
                            self.retired += 1;
                        } else {
                            self.free_hint = self.free_hint.min(collection.cursor + index);
                        }
                    }
                }
                drop(reclaimed_charge);
                collection.cursor = end;
                if end == self.slots.len() {
                    self.collections = self.collections.saturating_add(1);
                    return Some(CollectionStats {
                        before: collection.before,
                        retained: collection.retained,
                        reclaimed: collection.before - collection.retained,
                    });
                }
            }
        }
        None
    }

    fn mark(&mut self, index: usize, pending: &mut usize) {
        if !self.slots[index].marked {
            self.slots[index].marked = true;
            self.worklist[*pending] = index;
            *pending += 1;
        }
    }

    pub(crate) fn drop_spare_buffer(&mut self) {
        self.spare_buffer = None;
    }

    pub(crate) fn take_spare_bytes(&mut self, length: usize) -> Option<ByteBuffer> {
        let mut buf = self.spare_buffer.take()?;
        if length != 0
            && buf.data.capacity() == length
            && buf._charge.amount().bytes == length as u64
        {
            buf.data.clear();
            buf.heap = self.identity;
            Some(buf)
        } else {
            None
        }
    }

    pub(crate) fn try_recycle_sole_leaf(&mut self, old_ref: HeapRef, new_ref: HeapRef) {
        if self.allocated != 2 || old_ref == new_ref {
            return;
        }
        let old_idx = old_ref.slot as usize;
        let new_idx = new_ref.slot as usize;
        let valid = self.slots.get(old_idx).is_some_and(|s| {
            s.occupied
                && s.generation == old_ref.generation
                && matches!(s.layout, BYTES_LAYOUT | STRING_LAYOUT)
        }) && self
            .slots
            .get(new_idx)
            .is_some_and(|s| s.occupied && s.generation == new_ref.generation);
        if !valid {
            return;
        }
        if let Some(mut buffer) = self.buffers[old_idx].take() {
            self.buffer_bytes -= buffer._charge.amount().bytes;
            buffer.data.clear();
            buffer.heap = self.identity;
            self.spare_buffer = Some(buffer);
        }
        let slot = &mut self.slots[old_idx];
        slot.occupied = false;
        slot.marked = false;
        slot.generation = slot.generation.checked_add(1).unwrap_or(0);
        self.allocated -= 1;
        if slot.generation == 0 {
            self.retired += 1;
        } else {
            self.free_hint = self.free_hint.min(old_idx);
        }
    }

    pub(crate) fn prepare_bytes(&self, length: usize) -> Result<ByteBuffer, String> {
        if length > MAX_BYTES {
            return Err("byte buffer exceeds 65536 bytes".into());
        }
        let charge = self.account.reserve(Resources {
            bytes: length as u64,
            ..Resources::default()
        })?;
        let mut data = Vec::new();
        data.try_reserve_exact(length)
            .map_err(|error| error.to_string())?;
        if data.capacity() != length {
            return Err("allocator changed byte buffer capacity".into());
        }
        Ok(ByteBuffer {
            data,
            heap: self.identity,
            _charge: charge,
        })
    }

    pub(crate) fn bytes(&self, reference: HeapRef) -> Result<&[u8], String> {
        let slot = self.validate(reference)?;
        if slot.layout != BYTES_LAYOUT {
            return Err("expected a byte buffer".into());
        }
        Ok(&self.buffers[reference.slot as usize]
            .as_ref()
            .expect("occupied byte buffer")
            .data)
    }

    pub(crate) fn allocate_bytes_chunk(
        &mut self,
        buffer: &mut Option<ByteBuffer>,
        cursor: &mut usize,
    ) -> Result<Option<HeapRef>, String> {
        let data = buffer.as_ref().ok_or("missing byte buffer")?;
        if data.heap != self.identity {
            return Err("byte buffer belongs to a different heap".into());
        }
        if data.data.len() as u64 != data._charge.amount().bytes {
            return Err("incomplete byte buffer".into());
        }
        let end = (*cursor + WORK_CHUNK).min(self.slots.len());
        let found = if *cursor == 0
            && self.free_hint < end
            && !self.slots[self.free_hint].occupied
            && self.slots[self.free_hint].generation != 0
        {
            Some(self.free_hint)
        } else {
            self.slots[*cursor..end]
                .iter()
                .position(|slot| !slot.occupied && slot.generation != 0)
                .map(|offset| *cursor + offset)
        };
        let Some(index) = found else {
            *cursor = end;
            return if end == self.slots.len() {
                Err("record heap capacity exhausted".into())
            } else {
                Ok(None)
            };
        };
        let slot = &mut self.slots[index];
        slot.layout = BYTES_LAYOUT;
        slot.occupied = true;
        self.buffer_bytes += data._charge.amount().bytes;
        self.buffers[index] = buffer.take();
        self.allocated += 1;
        if self.free_hint == index {
            self.free_hint = index + 1;
        }
        Ok(Some(HeapRef {
            heap: self.identity,
            slot: index as u32,
            generation: slot.generation,
        }))
    }

    pub(crate) fn string_bytes(&self, reference: HeapRef) -> Result<&[u8], String> {
        if self.validate(reference)?.layout != STRING_LAYOUT {
            return Err("expected a UTF-8 string".into());
        }
        Ok(&self.buffers[reference.slot as usize]
            .as_ref()
            .expect("occupied string buffer")
            .data)
    }

    pub(crate) fn allocate_string_chunk(
        &mut self,
        buffer: &mut Option<ByteBuffer>,
        cursor: &mut usize,
    ) -> Result<Option<HeapRef>, String> {
        let reference = self.allocate_bytes_chunk(buffer, cursor)?;
        if let Some(reference) = reference {
            self.slots[reference.slot as usize].layout = STRING_LAYOUT;
        }
        Ok(reference)
    }

    fn schema(&self, layout: RecordLayout) -> Result<&Schema, String> {
        if layout.heap != self.identity || usize::from(layout.index) >= self.layout_count {
            return Err("record layout belongs to a different heap".into());
        }
        Ok(&self.schemas[usize::from(layout.index)])
    }

    fn validate(&self, reference: HeapRef) -> Result<&Slot, String> {
        if reference.heap != self.identity {
            return Err("record reference belongs to a different heap".into());
        }
        self.slots
            .get(reference.slot as usize)
            .filter(|slot| slot.occupied && slot.generation == reference.generation)
            .ok_or_else(|| "stale record reference".into())
    }

    fn decode_reference(&self, encoded: u64) -> HeapRef {
        HeapRef {
            heap: self.identity,
            slot: encoded as u32,
            generation: (encoded >> 32) as u32,
        }
    }
}

impl ShadowRoots {
    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub(crate) fn replace(&mut self, values: &[Option<HeapRef>]) {
        self.slots[..values.len()].copy_from_slice(values);
        self.slots[values.len()..self.count.max(values.len())].fill(None);
        self.count = values.len();
    }

    pub(crate) fn try_replace(&mut self, values: &[Option<HeapRef>]) -> Result<(), ()> {
        if values.len() > self.max_capacity {
            return Err(());
        }
        if values.len() > self.slots.len() {
            let next_cap = values.len().next_power_of_two().min(self.max_capacity);
            self.slots = allocate_array(next_cap, None).map_err(|_| ())?;
        }
        self.replace(values);
        Ok(())
    }

    pub fn new(heap: &RecordHeap, capacity: usize) -> Result<Self, String> {
        if !(1..=MAX_ROOTS).contains(&capacity) {
            return Err("shadow roots require 1..=64 slots".into());
        }
        Self::allocate(heap, capacity, capacity)
    }

    pub(crate) fn with_state_root(heap: &RecordHeap) -> Result<Self, String> {
        Self::allocate(heap, 4, MAX_ROOTS + 65)
    }

    fn allocate(
        heap: &RecordHeap,
        initial_capacity: usize,
        max_capacity: usize,
    ) -> Result<Self, String> {
        let bytes =
            std::mem::size_of::<Self>() + max_capacity * std::mem::size_of::<Option<HeapRef>>();
        let charge = heap.account.reserve(Resources {
            bytes: bytes as u64,
            ..Resources::default()
        })?;
        Ok(Self {
            heap: heap.identity,
            slots: allocate_array(initial_capacity, None)?,
            max_capacity,
            count: 0,
            charge,
        })
    }

    pub fn frame(&mut self) -> RootFrame<'_> {
        let base = self.count;
        RootFrame { roots: self, base }
    }

    pub fn charged_bytes(&self) -> u64 {
        self.charge.amount().bytes
    }
}

impl RootFrame<'_> {
    pub fn push(&mut self, heap: &RecordHeap, reference: HeapRef) -> Result<usize, String> {
        self.check_reference(heap, reference)?;
        if self.roots.count == self.roots.slots.len() {
            return Err("shadow root capacity exhausted".into());
        }
        let index = self.roots.count;
        self.roots.slots[index] = Some(reference);
        self.roots.count += 1;
        Ok(index - self.base)
    }

    pub fn set(
        &mut self,
        heap: &RecordHeap,
        index: usize,
        reference: HeapRef,
    ) -> Result<(), String> {
        if index >= self.roots.count - self.base {
            return Err("shadow root index out of bounds".into());
        }
        self.check_reference(heap, reference)?;
        self.roots.slots[self.base + index] = Some(reference);
        Ok(())
    }

    pub fn frame(&mut self) -> RootFrame<'_> {
        self.roots.frame()
    }

    pub fn collect(&self, heap: &mut RecordHeap) -> Result<CollectionStats, String> {
        heap.collect(self.roots)
    }

    fn check_reference(&self, heap: &RecordHeap, reference: HeapRef) -> Result<(), String> {
        if heap.identity != self.roots.heap {
            return Err("shadow roots belong to a different heap".into());
        }
        heap.validate(reference).map(|_| ())
    }
}

impl Drop for RootFrame<'_> {
    fn drop(&mut self) {
        self.roots.slots[self.base..self.roots.count].fill(None);
        self.roots.count = self.base;
    }
}

fn allocate_array<Value: Copy>(length: usize, value: Value) -> Result<Box<[Value]>, String> {
    let layout = Layout::array::<Value>(length).map_err(|error| error.to_string())?;
    if layout.size() == 0 {
        return Err("record storage must have a nonzero allocation size".into());
    }
    let pointer = unsafe { alloc(layout) }.cast::<Value>();
    if pointer.is_null() {
        return Err("record storage allocation failed".into());
    }
    for index in 0..length {
        unsafe { pointer.add(index).write(value) };
    }
    Ok(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(pointer, length)) })
}

fn next_identity() -> Result<u64, String> {
    let mut identity = NEXT_HEAP.load(Ordering::Relaxed);
    loop {
        let next = identity
            .checked_add(1)
            .ok_or("record heap identities exhausted")?;
        match NEXT_HEAP.compare_exchange_weak(identity, next, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => return Ok(identity),
            Err(current) => identity = current,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_leaf_ownership_transitive_roots_gc_and_generation_are_accounted() {
        let (vm, mut heap, mut roots, _) = fixture();
        roots.replace(&[]);
        heap.collect(&roots).unwrap();
        let baseline = vm.used();
        let mut buffer = heap.prepare_bytes(MAX_BYTES).unwrap();
        buffer.data.resize(MAX_BYTES, 255);
        assert_eq!(vm.used().bytes, baseline.bytes + MAX_BYTES as u64);
        let mut pending = Some(buffer);
        let bytes = heap
            .allocate_bytes_chunk(&mut pending, &mut 0)
            .unwrap()
            .unwrap();
        assert!(pending.is_none());
        assert_eq!(
            heap.stats().charged_bytes,
            baseline.bytes - roots.charged_bytes() + MAX_BYTES as u64
        );
        assert!(heap.get(bytes, 0).is_err());
        let linked = heap
            .register_layout(&[FieldType::Int, FieldType::Ref])
            .unwrap();
        let parent = heap
            .allocate(linked, &[RecordValue::Int(1), RecordValue::Ref(bytes)])
            .unwrap();
        roots.replace(&[Some(parent)]);
        assert_eq!(heap.collect(&roots).unwrap().retained, 2);
        assert_eq!(heap.bytes(bytes).unwrap(), vec![255; MAX_BYTES]);
        assert!(heap.bytes(parent).is_err());
        roots.replace(&[]);
        heap.collect(&roots).unwrap();
        assert!(heap.bytes(bytes).is_err());
        assert_eq!(vm.used(), baseline);
        let mut empty = Some(heap.prepare_bytes(0).unwrap());
        let current = heap
            .allocate_bytes_chunk(&mut empty, &mut 0)
            .unwrap()
            .unwrap();
        assert_ne!(current, bytes);
        assert!(heap.bytes(current).unwrap().is_empty());
        assert!(heap.prepare_bytes(MAX_BYTES + 1).is_err());
        drop((heap, roots));
        assert_eq!(vm.used(), Resources::default());
    }

    #[test]
    fn byte_reservation_rejection_at_each_level_and_unpublished_drop_roll_back() {
        for level in 0..3 {
            let limits = Resources {
                bytes: 1 << 20,
                ..Resources::default()
            };
            let vm = ResourceAccount::vm(limits);
            let tenant = vm.tenant(limits).unwrap();
            let account = tenant.process(limits).unwrap();
            let mut heap = RecordHeap::new(&account, 1, 1).unwrap();
            let levels = [&vm, &tenant, &account];
            let before = levels.map(|account| account.used());
            let guard = levels[level]
                .reserve(Resources {
                    bytes: limits.bytes - before[level].bytes - 1,
                    ..Resources::default()
                })
                .unwrap();
            let full = levels.map(|account| account.used());
            assert!(heap.prepare_bytes(2).is_err());
            assert_eq!(levels.map(|account| account.used()), full);
            drop(guard);
            let mut pending = heap.prepare_bytes(16).unwrap();
            pending.data.extend_from_slice(&[1; 8]);
            assert!(
                heap.allocate_bytes_chunk(&mut Some(pending), &mut 0)
                    .is_err()
            );
            assert_eq!(levels.map(|account| account.used()), before);
            assert_eq!(heap.stats().allocated, 0);
            drop(heap);
            assert_eq!(vm.used(), Resources::default());
        }
    }

    #[test]
    fn byte_builder_cannot_transfer_its_original_heap_or_account_charge() {
        let limit = Resources {
            bytes: 1 << 20,
            ..Resources::default()
        };
        let vm = ResourceAccount::vm(limit);
        let tenant = vm.tenant(limit).unwrap();
        let source_account = tenant.process(limit).unwrap();
        let target_account = tenant.process(limit).unwrap();
        let mut source = RecordHeap::new(&source_account, 1, 1).unwrap();
        let mut target = RecordHeap::new(&target_account, 1, 1).unwrap();
        let baseline = vm.used();
        let target_before = target_account.used();
        let mut data = source.prepare_bytes(16).unwrap();
        data.data.resize(16, 7);
        let mut pending = Some(data);
        assert!(target.allocate_bytes_chunk(&mut pending, &mut 0).is_err());
        assert!(pending.is_some());
        assert_eq!(target_account.used(), target_before);
        assert_eq!(target.stats().allocated, 0);
        assert_eq!(vm.used().bytes, baseline.bytes + 16);
        let reference = source
            .allocate_bytes_chunk(&mut pending, &mut 0)
            .unwrap()
            .unwrap();
        assert_eq!(source.bytes(reference).unwrap(), [7; 16]);
        assert!(pending.is_none());
        drop(source);
        drop(target);
        assert_eq!(vm.used(), Resources::default());
    }

    fn fixture() -> (ResourceAccount, RecordHeap, ShadowRoots, HeapRef) {
        let limit = Resources {
            bytes: 1 << 20,
            ..Resources::default()
        };
        let vm = ResourceAccount::vm(limit);
        let tenant = vm.tenant(limit).unwrap();
        let account = tenant.process(limit).unwrap();
        let mut heap = RecordHeap::new(&account, 64, 2).unwrap();
        let scalar = heap.register_layout(&[FieldType::Int]).unwrap();
        let linked = heap
            .register_layout(&[FieldType::Int, FieldType::Ref])
            .unwrap();
        let mut head = heap.allocate(scalar, &[RecordValue::Int(0)]).unwrap();
        for value in 1..32 {
            head = heap
                .allocate(linked, &[RecordValue::Int(value), RecordValue::Ref(head)])
                .unwrap();
        }
        for value in 32..64 {
            heap.allocate(scalar, &[RecordValue::Int(value)]).unwrap();
        }
        let mut roots = ShadowRoots::new(&heap, 1).unwrap();
        roots.replace(&[Some(head)]);
        (vm, heap, roots, head)
    }

    #[test]
    fn interrupted_collection_restarts_from_each_phase_with_live_graph_and_exact_counts() {
        for chunks in 0..15 {
            let (vm, mut heap, roots, head) = fixture();
            let used = vm.used();
            let mut collection = heap.begin_collection(&roots).unwrap();
            for _ in 0..chunks {
                let phase = collection.phase;
                let cursor = collection.cursor;
                let before = heap.slots.iter().filter(|slot| slot.marked).count();
                if heap.collect_chunk(&roots, &mut collection).is_some() {
                    break;
                }
                if collection.phase == phase && phase != CollectionPhase::Trace {
                    assert!(collection.cursor - cursor <= WORK_CHUNK);
                }
                if phase == CollectionPhase::Trace {
                    assert!(
                        heap.slots.iter().filter(|slot| slot.marked).count() - before <= WORK_CHUNK
                    );
                }
                assert_eq!(
                    heap.stats().allocated,
                    heap.slots.iter().filter(|slot| slot.occupied).count()
                );
            }
            assert_eq!(heap.get(head, 0).unwrap(), RecordValue::Int(31));
            let stats = heap.collect(&roots).unwrap();
            assert_eq!(stats.retained, 32);
            assert_eq!(heap.stats().allocated, 32);
            assert_eq!(vm.used(), used);
            drop((heap, roots));
            assert_eq!(vm.used(), Resources::default());
        }
    }

    #[test]
    fn chunked_sweep_retires_exhausted_generations_and_allocator_scans_fixed_windows() {
        let (vm, mut heap, mut roots, _) = fixture();
        let scalar = heap.register_layout(&[FieldType::Int]).unwrap();
        heap.slots[63].generation = u32::MAX;
        roots.replace(&[]);
        heap.collect(&roots).unwrap();
        assert_eq!(heap.stats().retired, 1);
        assert_eq!(heap.stats().allocated, 0);
        let mut reference = None;
        for value in 0..63 {
            reference = Some(heap.allocate(scalar, &[RecordValue::Int(value)]).unwrap());
        }
        let mut cursor = 0;
        for expected in [16, 32, 48] {
            assert_eq!(
                heap.allocate_chunk(scalar, &[RecordValue::Int(99)], &mut cursor)
                    .unwrap(),
                None
            );
            assert_eq!(cursor, expected);
        }
        assert!(
            heap.allocate_chunk(scalar, &[RecordValue::Int(99)], &mut cursor)
                .is_err()
        );
        roots.replace(&[reference]);
        heap.collect(&roots).unwrap();
        assert_eq!(heap.stats().allocated, 1);
        assert_eq!(heap.stats().retired, 1);
        drop((heap, roots));
        assert_eq!(vm.used(), Resources::default());
    }
}
